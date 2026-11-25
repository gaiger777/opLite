//! 흘려 보내는 OVA(ustar, 계획 72 §12.3). 내보내기 디렉터리의 파일들을 서술자 → 매니페스트 → 디스크 차례(DSP0243)로
//! 그 자리에서 tar로 묶는다. 크기를 미리 알므로 [`ova_size`]가 정확한 `Content-Length`다. 8 GiB를 넘는 파일은 pax 확장
//! 머리(`size=`)와 base-256 크기 칸을 함께 쓴다(pax를 아는 읽개·GNU 읽개 모두 — 계획 29의 OVA 읽개도 둘 다 읽는다).

use std::path::PathBuf;

use bytes::Bytes;
use opl_peer::{DownloadPiece, DownloadStream};
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

pub const BLOCK: u64 = 512;
/// 8진수 11자리 크기 칸에 들어가는 가장 큰 값(8 GiB - 1)
const OCTAL_MAX: u64 = 0o777_7777_7777;
/// 읽는 조각 크기
const CHUNK: usize = 1 << 20;

fn pad(n: u64) -> u64 {
    n.next_multiple_of(BLOCK) - n
}

/// pax 레코드 `"{len} size={size}\n"`(길이는 자기 자리 수를 포함한다)
fn pax_size_record(size: u64) -> String {
    let body = format!(" size={size}\n");
    let mut len = body.len() + 1;
    loop {
        let candidate = format!("{len}{body}");
        if candidate.len() == len {
            return candidate;
        }
        len = candidate.len();
    }
}

/// 8진수 칸(`width`자리 + NUL)
fn octal(field: &mut [u8], value: u64) {
    let digits = field.len() - 1;
    let text = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(text.as_bytes());
    field[digits] = 0;
}

/// 머리 블록 하나. `size`가 8진수 칸을 넘으면 base-256으로 쓴다.
fn header_block(name: &str, size: u64, mtime: u64, typeflag: u8) -> [u8; BLOCK as usize] {
    let mut h = [0u8; BLOCK as usize];
    let n = name.as_bytes();
    h[..n.len().min(100)].copy_from_slice(&n[..n.len().min(100)]);
    octal(&mut h[100..108], 0o644);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    if size > OCTAL_MAX {
        h[124] = 0x80;
        h[128..136].copy_from_slice(&size.to_be_bytes());
    } else {
        octal(&mut h[124..136], size);
    }
    octal(&mut h[136..148], mtime.min(OCTAL_MAX));
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    // 체크섬: 칸을 공백으로 두고 더한다. 6자리 8진수 + NUL + 공백
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
    let text = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(text.as_bytes());
    h
}

/// 파일 하나의 머리(큰 파일이면 pax 머리와 그 본문 포함)
pub fn entry_header(name: &str, size: u64, mtime: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(BLOCK as usize * 3);
    if size > OCTAL_MAX {
        let record = pax_size_record(size);
        let pax_name = format!("PaxHeaders/{name}");
        out.extend_from_slice(&header_block(&pax_name, record.len() as u64, mtime, b'x'));
        out.extend_from_slice(record.as_bytes());
        out.resize(out.len() + pad(record.len() as u64) as usize, 0);
    }
    out.extend_from_slice(&header_block(name, size, mtime, b'0'));
    out
}

/// 파일 하나가 OVA에서 차지하는 바이트(머리 + 본문 + 채움)
pub fn entry_bytes(name: &str, size: u64) -> u64 {
    entry_header(name, size, 0).len() as u64 + size + pad(size)
}

/// 이 파일들(차례대로 `(이름, 크기)`)의 OVA 크기: 항목들 + 끝 0 블록 둘
pub fn ova_size<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> u64 {
    files
        .into_iter()
        .map(|(name, size)| entry_bytes(name, size))
        .sum::<u64>()
        + 2 * BLOCK
}

/// 흘려 보낼 파일 하나
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub path: PathBuf,
    /// 기록된 크기. 읽은 바이트가 다르면(파일이 바뀌었다) 흐름을 실패로 끝낸다.
    pub size: u64,
}

/// 파일 `path`를 `size`바이트 그대로 보낸다. 다르면 오류
async fn send_file(
    tx: &mpsc::Sender<Result<DownloadPiece, String>>,
    part: &Part,
) -> Result<bool, String> {
    let mut file = tokio::fs::File::open(&part.path)
        .await
        .map_err(|e| format!("cannot open {}: {e}", part.name))?;
    let mut sent = 0u64;
    loop {
        let mut buf = vec![0u8; CHUNK];
        let n = file
            .read(&mut buf)
            .await
            .map_err(|e| format!("cannot read {}: {e}", part.name))?;
        if n == 0 {
            break;
        }
        sent += n as u64;
        if sent > part.size {
            return Err(format!(
                "{} changed since the export was written",
                part.name
            ));
        }
        buf.truncate(n);
        if tx
            .send(Ok(DownloadPiece::Data(Bytes::from(buf))))
            .await
            .is_err()
        {
            return Ok(false);
        }
    }
    if sent != part.size {
        return Err(format!(
            "{} changed since the export was written",
            part.name
        ));
    }
    Ok(true)
}

/// 흐름을 띄운다. `tar`면 OVA로 묶고(항목 수로 끝냄), 아니면 파일 하나(`parts`의 첫째)를 그대로 보낸다.
pub fn spawn(parts: Vec<Part>, tar: bool, mtime: u64) -> DownloadStream {
    let (tx, rx) = mpsc::channel(4);
    tokio::spawn(async move {
        let entries = parts.len() as u64;
        let run = async {
            for part in &parts {
                if tar {
                    let mut head = entry_header(&part.name, part.size, mtime);
                    head.shrink_to_fit();
                    if tx.send(Ok(DownloadPiece::Data(head.into()))).await.is_err() {
                        return Ok(false);
                    }
                }
                if !send_file(&tx, part).await? {
                    return Ok(false);
                }
                let fill = pad(part.size);
                if tar
                    && fill > 0
                    && tx
                        .send(Ok(DownloadPiece::Data(Bytes::from(vec![
                            0u8;
                            fill as usize
                        ]))))
                        .await
                        .is_err()
                {
                    return Ok(false);
                }
                if !tar {
                    break;
                }
            }
            // ustar 끝 표시(0 블록 둘). 받는 쪽이 끊었으면 false
            let sent = !tar
                || tx
                    .send(Ok(DownloadPiece::Data(Bytes::from(vec![
                        0u8;
                        2 * BLOCK as usize
                    ]))))
                    .await
                    .is_ok();
            Ok::<_, String>(sent)
        };
        match run.await {
            Ok(true) => {
                let _ = tx
                    .send(Ok(DownloadPiece::Done {
                        entries: if tar { entries } else { 1 },
                    }))
                    .await;
            }
            Ok(false) => {}
            Err(e) => {
                let _ = tx.send(Err(e)).await;
            }
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm_imports::ova::{parse_ova, read_index};

    #[test]
    fn pax_records_count_their_own_length() {
        for size in [0u64, 9, 99_999, 9 << 30, u64::MAX] {
            let r = pax_size_record(size);
            let (len, rest) = r.split_once(' ').unwrap();
            assert_eq!(len.parse::<usize>().unwrap(), r.len(), "{r:?}");
            assert_eq!(rest, format!("size={size}\n"));
        }
    }

    #[test]
    fn headers_are_valid_ustar() {
        let h = entry_header("web.ovf", 1234, 1_700_000_000);
        assert_eq!(h.len(), 512);
        assert_eq!(&h[..7], b"web.ovf");
        assert_eq!(&h[124..136], b"00000002322\0");
        assert_eq!(&h[257..263], b"ustar\0");
        assert_eq!(h[156], b'0');
        // 체크섬은 칸을 공백으로 둔 합이다.
        let stored = u64::from_str_radix(std::str::from_utf8(&h[148..154]).unwrap(), 8).unwrap();
        let mut copy = h.clone();
        copy[148..156].fill(b' ');
        assert_eq!(stored, copy.iter().map(|&b| u64::from(b)).sum::<u64>());

        // 8 GiB를 넘으면 pax 머리 + base-256 크기 칸
        let big = 9u64 << 30;
        let h = entry_header("web-disk0.vmdk", big, 0);
        assert_eq!(h.len(), 3 * 512);
        assert_eq!(h[156], b'x');
        assert!(h[..100].starts_with(b"PaxHeaders/web-disk0.vmdk"));
        assert!(
            std::str::from_utf8(&h[512..1024])
                .unwrap()
                .contains(&format!("size={big}\n"))
        );
        let main = &h[1024..];
        assert_eq!(main[124], 0x80);
        assert_eq!(u64::from_be_bytes(main[128..136].try_into().unwrap()), big);
        assert_eq!(entry_bytes("web-disk0.vmdk", big), 3 * 512 + big);
    }

    async fn collect(mut rx: DownloadStream) -> (Vec<u8>, Option<u64>, Option<String>) {
        let mut out = Vec::new();
        let (mut done, mut failed) = (None, None);
        while let Some(piece) = rx.recv().await {
            match piece {
                Ok(DownloadPiece::Data(d)) => out.extend_from_slice(&d),
                Ok(DownloadPiece::Done { entries }) => done = Some(entries),
                Err(e) => failed = Some(e),
            }
        }
        (out, done, failed)
    }

    /// 흘린 바이트 수 = [`ova_size`], 계획 29의 OVA 읽개가 항목 차례·크기·본문을 그대로 읽고 OVA로 해석한다.
    #[tokio::test]
    async fn stream_size_matches_and_the_ova_reader_reads_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let disk = vec![0x5au8; 1000];
        let spec: opl_types::vm::VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 512,
            "machine": "q35",
            "disks": [{ "volume": "web-disk0", "bus": "virtio-blk" }],
            "nics": [{ "network": "lan", "model": "virtio" }],
        }))
        .unwrap();
        let ovf = crate::vm_imports::ovf_write::write_ovf(&crate::vm_imports::ovf_write::OvfVm {
            name: "web",
            spec: &spec,
            disks: &[crate::vm_imports::ovf_write::OvfDisk {
                file: "web-disk0.vmdk".into(),
                size: disk.len() as u64,
                capacity: 1 << 20,
                populated: None,
                bus: opl_types::vm::DiskBus::VirtioBlk,
            }],
            guest_os: "otherGuest64",
            hw_version: 14,
            keep_macs: false,
        });
        let sha = |b: &[u8]| hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b));
        let mf = crate::vm_imports::ovf_write::write_manifest([
            ("web.ovf", sha(ovf.as_bytes()).as_str()),
            ("web-disk0.vmdk", sha(&disk).as_str()),
        ]);
        let files: Vec<(String, Vec<u8>)> = vec![
            ("web.ovf".into(), ovf.into_bytes()),
            ("web.mf".into(), mf.into_bytes()),
            ("web-disk0.vmdk".into(), disk.clone()),
        ];
        let mut parts = Vec::new();
        for (name, data) in &files {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            parts.push(Part {
                name: name.clone(),
                path,
                size: data.len() as u64,
            });
        }
        let want = ova_size(parts.iter().map(|p| (p.name.as_str(), p.size)));
        let (bytes, done, failed) = collect(spawn(parts.clone(), true, 1_700_000_000)).await;
        assert_eq!(failed, None);
        assert_eq!(done, Some(3));
        assert_eq!(bytes.len() as u64, want);
        assert_eq!(bytes.len() % 512, 0);
        let ova = dir.path().join("out.ova");
        std::fs::write(&ova, &bytes).unwrap();
        let index = read_index(&ova).unwrap();
        let names: Vec<&str> = index.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["web.ovf", "web.mf", "web-disk0.vmdk"]);
        let e = index.get("web-disk0.vmdk").unwrap();
        assert_eq!(
            &bytes[e.offset as usize..(e.offset + e.size) as usize],
            disk.as_slice()
        );
        // OVA로 해석하면 쓴 VM이 나오고, 매니페스트가 디스크를 확인한다(확인하지 못한다는 경고가 없다).
        let parsed = parse_ova(&ova).unwrap();
        assert_eq!(parsed.vm.name, "web");
        assert_eq!(parsed.vm.disks[0].file, "web-disk0.vmdk");
        assert_eq!(parsed.warnings, Vec::<String>::new());
        let mf = &parsed.manifest["web-disk0.vmdk"];
        let out = dir.path().join("disk");
        crate::vm_imports::ova::extract(&ova, e, &out, Some(mf)).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), disk);

        // 파일 하나(OVF 폴더로 받기)
        let (one, done, _) = collect(spawn(vec![parts[2].clone()], false, 0)).await;
        assert_eq!(one, disk);
        assert_eq!(done, Some(1));

        // 기록과 크기가 다르면 실패로 끝난다(잘린 OVA를 성공처럼 보내지 않는다).
        let mut wrong = parts.clone();
        wrong[2].size += 1;
        let (_, done, failed) = collect(spawn(wrong, true, 0)).await;
        assert_eq!(done, None);
        assert!(failed.unwrap().contains("changed"));
    }
}
