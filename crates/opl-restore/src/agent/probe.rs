//! 볼륨 찾기의 순수한 부분(설계 §9.1 탐지·§9.3 이름): `blkid -p -o export` 출력, `lvm lvs`·`lvm pvs`의 JSON 보고,
//! virtio 디스크의 serial(`opl-disk{i}`)을 해석해 [`Volume`] 목록을 만든다. 명령을 부르고 sysfs를 읽는 쪽은
//! `agent::linux`(도우미 VM 안)다.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::mount::mountability;
use crate::proto::{
    MAX_LABEL, MAX_VOLUMES, Partition, PartitionTable, Volume, VolumeKind, disk_volume_id,
    lv_volume_id, partition_volume_id,
};

/// `blkid -p -o export`의 `KEY=value` 줄들. 값의 `\x`는 `x`로 푼다(blkid가 공백·따옴표 따위를 이렇게 쓴다).
pub fn parse_blkid_export(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
            continue;
        }
        let mut v = String::with_capacity(value.len());
        let mut chars = value.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    v.push(n);
                }
            } else {
                v.push(c);
            }
        }
        out.insert(key.to_string(), v);
    }
    out
}

/// virtio 디스크 serial `opl-disk{i}` → 백업 디스크 번호
pub fn disk_index(serial: &str) -> Option<u32> {
    let n = serial.trim().strip_prefix("opl-disk")?;
    n.parse::<u32>().ok().filter(|i| i.to_string() == n)
}

/// 블록 장치 하나의 탐지 결과
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlockProbe {
    /// `/dev/vda1`
    pub dev: String,
    /// 바이트
    pub size: u64,
    /// blkid 보고
    pub blkid: BTreeMap<String, String>,
}

/// 디스크 하나와 그 파티션들(커널이 만든 `vdXN`, 번호는 sysfs `partition`)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiskProbe {
    pub index: u32,
    pub disk: BlockProbe,
    pub partitions: Vec<(u32, BlockProbe)>,
}

/// `lvm lvs --reportformat json --units b --nosuffix -o vg_name,lv_name,vg_uuid,lv_uuid,lv_size,lv_attr,lv_dm_path`의 줄
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LvReport {
    pub vg_name: String,
    pub lv_name: String,
    #[serde(default)]
    pub vg_uuid: String,
    #[serde(default)]
    pub lv_uuid: String,
    #[serde(default)]
    pub lv_size: String,
    #[serde(default)]
    pub lv_attr: String,
    #[serde(default)]
    pub lv_dm_path: String,
}

/// `lvm pvs --reportformat json -o pv_name,vg_name`의 줄
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PvReport {
    pub pv_name: String,
    #[serde(default)]
    pub vg_name: String,
}

#[derive(Deserialize)]
struct Report<T> {
    report: Vec<ReportPart<T>>,
}

#[derive(Deserialize)]
struct ReportPart<T> {
    #[serde(default = "Vec::new", alias = "lv", alias = "pv")]
    rows: Vec<T>,
}

fn parse_report<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>, String> {
    let r: Report<T> = serde_json::from_str(text).map_err(|e| format!("lvm report: {e}"))?;
    Ok(r.report.into_iter().flat_map(|p| p.rows).collect())
}

pub fn parse_lvs_json(text: &str) -> Result<Vec<LvReport>, String> {
    parse_report(text)
}

pub fn parse_pvs_json(text: &str) -> Result<Vec<PvReport>, String> {
    parse_report(text)
}

/// 논리 볼륨 하나와 그 blkid 보고
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvProbe {
    pub report: LvReport,
    pub blkid: BTreeMap<String, String>,
}

/// 찾은 볼륨과 그 장치(마운트할 때)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub volume: Volume,
    pub dev: String,
}

/// 게스트 값을 규약 길이 안으로(UTF-8 경계에서 자름)
pub fn clip(s: &str) -> String {
    if s.len() <= MAX_LABEL {
        return s.to_string();
    }
    let mut end = MAX_LABEL;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn field(blkid: &BTreeMap<String, String>, key: &str) -> Option<String> {
    blkid.get(key).filter(|v| !v.is_empty()).map(|v| clip(v))
}

/// MBR 확장 파티션(논리 파티션을 담는 틀 — 볼륨이 아님)
fn is_extended(blkid: &BTreeMap<String, String>) -> bool {
    blkid.get("PART_ENTRY_SCHEME").map(String::as_str) == Some("dos")
        && matches!(
            blkid.get("PART_ENTRY_TYPE").map(String::as_str),
            Some("0x5" | "0x05" | "0xf" | "0x0f" | "0x85")
        )
}

const GPT_TYPES: &[(&str, &str)] = &[
    ("0fc63daf-8483-4772-8e79-3d69d8477de4", "Linux filesystem"),
    (
        "4f68bce3-e8cd-4db1-96e7-fbcaf984b709",
        "Linux root (x86-64)",
    ),
    (
        "b921b045-1df0-41c3-af44-4c6f280d3fae",
        "Linux root (ARM-64)",
    ),
    ("933ac7e1-2eb4-4f13-b844-0e14e2aef915", "Linux home"),
    ("e6d6d379-f507-44c2-a23c-238f2a3df928", "Linux LVM"),
    ("0657fd6d-a4ab-43c4-84e5-0933c84b4f4f", "Linux swap"),
    ("a19d880f-05fc-4d3b-a006-743f0f84911e", "Linux RAID"),
    (
        "bc13c2ff-59e6-4262-a352-b275fd6f7172",
        "Linux extended boot",
    ),
    ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "EFI System"),
    ("21686148-6449-6e6f-744e-656564454649", "BIOS boot"),
    (
        "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
        "Microsoft basic data",
    ),
    ("e3c9e316-0b5c-4db8-817d-f92df00215ae", "Microsoft reserved"),
    (
        "de94bba4-06d1-4d40-a16a-bfd50179d6ac",
        "Windows recovery environment",
    ),
];

const MBR_TYPES: &[(u8, &str)] = &[
    (0x01, "FAT12"),
    (0x04, "FAT16 <32M"),
    (0x06, "FAT16"),
    (0x07, "HPFS/NTFS/exFAT"),
    (0x0b, "W95 FAT32"),
    (0x0c, "W95 FAT32 (LBA)"),
    (0x0e, "W95 FAT16 (LBA)"),
    (0x27, "Hidden NTFS WinRE"),
    (0x82, "Linux swap"),
    (0x83, "Linux"),
    (0x8e, "Linux LVM"),
    (0xef, "EFI (FAT-12/16/32)"),
    (0xfd, "Linux raid autodetect"),
];

/// 파티션 종류 이름(`Linux filesystem`, `Linux (0x83)`, 모르면 원래 값)
pub fn partition_type_name(table: PartitionTable, raw: &str) -> String {
    match table {
        PartitionTable::Gpt => {
            let lower = raw.to_ascii_lowercase();
            GPT_TYPES
                .iter()
                .find(|(g, _)| *g == lower)
                .map_or_else(|| clip(raw), |(_, n)| (*n).to_string())
        }
        PartitionTable::Mbr => {
            let n = raw
                .strip_prefix("0x")
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            match n.and_then(|n| MBR_TYPES.iter().find(|(t, _)| *t == n)) {
                Some((t, name)) => format!("{name} (0x{t:02x})"),
                None => clip(raw),
            }
        }
    }
}

fn fs_volume(
    id: String,
    disk: u32,
    kind: VolumeKind,
    size: u64,
    partition: Option<Partition>,
    blkid: &BTreeMap<String, String>,
) -> Volume {
    let fs = field(blkid, "TYPE");
    let (mountable, note) = mountability(fs.as_deref());
    Volume {
        id,
        disk,
        kind,
        size,
        partition,
        fs,
        label: field(blkid, "LABEL"),
        uuid: field(blkid, "UUID"),
        mountable,
        note,
    }
}

/// 디스크·파티션·LV 탐지 결과 → 볼륨 목록. 파티션 표가 있는 디스크는 파티션들이(파티션이 없으면 디스크가 "빈 파티션
/// 표"로), 없는 디스크는 디스크 전체가 볼륨이다. LV는 VG의 첫 PV가 있는 디스크에 붙는다. [`MAX_VOLUMES`]에서 자른다.
pub fn build_volumes(disks: &[DiskProbe], pvs: &[PvReport], lvs: &[LvProbe]) -> Vec<Found> {
    let mut out = Vec::new();
    let mut dev_disk: BTreeMap<&str, u32> = BTreeMap::new();
    let mut disks: Vec<&DiskProbe> = disks.iter().collect();
    disks.sort_by_key(|d| d.index);
    for d in disks {
        dev_disk.insert(&d.disk.dev, d.index);
        let parts: Vec<&(u32, BlockProbe)> = {
            let mut p: Vec<_> = d
                .partitions
                .iter()
                .filter(|(_, b)| !is_extended(&b.blkid))
                .collect();
            p.sort_by_key(|(n, _)| *n);
            p
        };
        let has_table = d.disk.blkid.contains_key("PTTYPE") || !d.partitions.is_empty();
        if !has_table {
            out.push(Found {
                volume: fs_volume(
                    disk_volume_id(d.index),
                    d.index,
                    VolumeKind::Disk,
                    d.disk.size,
                    None,
                    &d.disk.blkid,
                ),
                dev: d.disk.dev.clone(),
            });
            continue;
        }
        if parts.is_empty() {
            let mut v = fs_volume(
                disk_volume_id(d.index),
                d.index,
                VolumeKind::Disk,
                d.disk.size,
                None,
                &BTreeMap::new(),
            );
            v.note = Some("empty partition table".to_string());
            out.push(Found {
                volume: v,
                dev: d.disk.dev.clone(),
            });
            continue;
        }
        for (number, p) in parts {
            dev_disk.insert(&p.dev, d.index);
            let table = match p.blkid.get("PART_ENTRY_SCHEME").map(String::as_str) {
                Some("gpt") => PartitionTable::Gpt,
                _ => match d.disk.blkid.get("PTTYPE").map(String::as_str) {
                    Some("gpt") => PartitionTable::Gpt,
                    _ => PartitionTable::Mbr,
                },
            };
            let partition = Partition {
                table,
                number: *number,
                type_name: p
                    .blkid
                    .get("PART_ENTRY_TYPE")
                    .map(|t| partition_type_name(table, t))
                    .unwrap_or_default(),
                uuid: (table == PartitionTable::Gpt)
                    .then(|| field(&p.blkid, "PART_ENTRY_UUID"))
                    .flatten(),
            };
            out.push(Found {
                volume: fs_volume(
                    partition_volume_id(d.index, *number),
                    d.index,
                    VolumeKind::Partition,
                    p.size,
                    Some(partition),
                    &p.blkid,
                ),
                dev: p.dev.clone(),
            });
        }
    }
    // VG → 첫 PV의 디스크
    let mut vg_disk: BTreeMap<&str, u32> = BTreeMap::new();
    let mut pvs: Vec<&PvReport> = pvs.iter().collect();
    pvs.sort_by(|a, b| a.pv_name.cmp(&b.pv_name));
    for pv in pvs {
        if let Some(disk) = dev_disk.get(pv.pv_name.as_str()) {
            vg_disk.entry(pv.vg_name.as_str()).or_insert(*disk);
        }
    }
    let mut lv_found = Vec::new();
    for lv in lvs {
        let r = &lv.report;
        // 씬 풀·VDO 풀은 파일시스템이 아니다.
        if matches!(r.lv_attr.chars().next(), Some('t' | 'd')) {
            continue;
        }
        let Some(id) = lv_volume_id(&r.vg_name, &r.lv_name, &r.vg_uuid, &r.lv_uuid) else {
            continue;
        };
        let active = r.lv_attr.chars().nth(4) == Some('a') && !r.lv_dm_path.is_empty();
        let mut v = fs_volume(
            id,
            vg_disk.get(r.vg_name.as_str()).copied().unwrap_or(0),
            VolumeKind::LogicalVolume,
            r.lv_size.trim().parse().unwrap_or(0),
            None,
            &lv.blkid,
        );
        if !active {
            v.mountable = false;
            v.note = Some("logical volume could not be activated".to_string());
        }
        lv_found.push(Found {
            volume: v,
            dev: r.lv_dm_path.clone(),
        });
    }
    lv_found.sort_by(|a, b| a.volume.id.cmp(&b.volume.id));
    lv_found.dedup_by(|a, b| a.volume.id == b.volume.id);
    out.extend(lv_found);
    out.truncate(MAX_VOLUMES);
    out
}

#[cfg(test)]
mod tests;
