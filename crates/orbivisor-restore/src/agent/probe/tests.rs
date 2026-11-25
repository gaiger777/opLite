use super::*;
use crate::proto::check_volumes;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/probe/",
            $name
        ))
    };
}

fn block(dev: &str, size: u64, text: &str) -> BlockProbe {
    BlockProbe {
        dev: dev.into(),
        size,
        blkid: parse_blkid_export(text),
    }
}

fn disks() -> Vec<DiskProbe> {
    vec![
        // 순서를 섞어 둔다(번호 차례로 정렬되는지)
        DiskProbe {
            index: 1,
            disk: block("/dev/vdb", 1 << 30, fixture!("vdb.txt")),
            partitions: vec![
                (6, block("/dev/vdb6", 428_867_584, fixture!("vdb6.txt"))),
                (1, block("/dev/vdb1", 104_857_600, fixture!("vdb1.txt"))),
                (2, block("/dev/vdb2", 1024, fixture!("vdb2.txt"))),
                (5, block("/dev/vdb5", 536_870_912, fixture!("vdb5.txt"))),
            ],
        },
        DiskProbe {
            index: 0,
            disk: block("/dev/vda", 20 << 30, fixture!("vda.txt")),
            partitions: vec![
                (1, block("/dev/vda1", 629_145_600, fixture!("vda1.txt"))),
                (2, block("/dev/vda2", 1 << 30, fixture!("vda2.txt"))),
                (3, block("/dev/vda3", 19_764_609_024, fixture!("vda3.txt"))),
            ],
        },
        DiskProbe {
            index: 2,
            disk: block("/dev/vdc", 2 << 30, fixture!("vdc.txt")),
            partitions: vec![],
        },
        DiskProbe {
            index: 3,
            disk: block("/dev/vdd", 2 << 30, fixture!("vdd.txt")),
            partitions: vec![],
        },
    ]
}

fn lvs() -> Vec<LvProbe> {
    parse_lvs_json(fixture!("lvs.json"))
        .unwrap()
        .into_iter()
        .map(|report| LvProbe {
            blkid: match report.lv_name.as_str() {
                "root" => parse_blkid_export("TYPE=xfs\nUUID=aaaa-bbbb\nUSAGE=filesystem\n"),
                "swap" => parse_blkid_export("TYPE=swap\nUUID=cccc\n"),
                "thin1" => parse_blkid_export("TYPE=ext4\nLABEL=data\n"),
                _ => Default::default(),
            },
            report,
        })
        .collect()
}

#[test]
fn blkid_export_lines() {
    let m = parse_blkid_export(fixture!("vdb1.txt"));
    assert_eq!(m["LABEL"], "my data \"q\"");
    assert_eq!(m["TYPE"], "ext4");
    assert_eq!(m["PART_ENTRY_NUMBER"], "1");
    assert!(parse_blkid_export("").is_empty());
    assert!(parse_blkid_export("garbage\nlower=x\n=y\n").is_empty());
    assert_eq!(
        parse_blkid_export("PART_ENTRY_NAME=EFI\\ System")["PART_ENTRY_NAME"],
        "EFI System"
    );
}

#[test]
fn lvm_reports() {
    let lvs = parse_lvs_json(fixture!("lvs.json")).unwrap();
    assert_eq!(lvs.len(), 5);
    assert_eq!(lvs[0].lv_dm_path, "/dev/mapper/rl-root");
    assert_eq!(lvs[0].lv_size, "8585740288");
    let pvs = parse_pvs_json(fixture!("pvs.json")).unwrap();
    assert_eq!(
        pvs,
        [PvReport {
            pv_name: "/dev/vda3".into(),
            vg_name: "rl".into()
        }]
    );
    assert!(parse_lvs_json("{").is_err());
    assert!(parse_lvs_json(r#"{"report":[{}]}"#).unwrap().is_empty());
}

#[test]
fn disk_serials() {
    assert_eq!(disk_index("opl-disk0\n"), Some(0));
    assert_eq!(disk_index("opl-disk12"), Some(12));
    for bad in ["opl-disk", "opl-disk01", "disk0", "opl-disk-1", "opl-diskx"] {
        assert_eq!(disk_index(bad), None, "{bad}");
    }
}

#[test]
fn partition_type_names() {
    assert_eq!(
        partition_type_name(PartitionTable::Gpt, "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"),
        "EFI System"
    );
    assert_eq!(partition_type_name(PartitionTable::Gpt, "1234"), "1234");
    assert_eq!(
        partition_type_name(PartitionTable::Mbr, "0x83"),
        "Linux (0x83)"
    );
    assert_eq!(
        partition_type_name(PartitionTable::Mbr, "0x7"),
        "HPFS/NTFS/exFAT (0x07)"
    );
    assert_eq!(partition_type_name(PartitionTable::Mbr, "0x99"), "0x99");
}

#[test]
fn volumes_from_fixtures() {
    let pvs = parse_pvs_json(fixture!("pvs.json")).unwrap();
    let found = build_volumes(&disks(), &pvs, &lvs());
    let vols: Vec<Volume> = found.iter().map(|f| f.volume.clone()).collect();
    check_volumes(&vols).unwrap();
    type Row<'a> = (&'a str, u32, Option<&'a str>, bool, Option<&'a str>);
    let summary: Vec<Row> = vols
        .iter()
        .map(|v| {
            (
                v.id.as_str(),
                v.disk,
                v.fs.as_deref(),
                v.mountable,
                v.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("disk0p1", 0, Some("vfat"), true, None),
            ("disk0p2", 0, Some("xfs"), true, None),
            (
                "disk0p3",
                0,
                Some("LVM2_member"),
                false,
                Some("LVM physical volume")
            ),
            ("disk1p1", 1, Some("ext4"), true, None),
            ("disk1p5", 1, Some("ntfs"), true, None),
            (
                "disk1p6",
                1,
                Some("crypto_LUKS"),
                false,
                Some("encrypted (LUKS) — not supported")
            ),
            ("disk2", 2, Some("btrfs"), true, None),
            ("disk3", 3, None, false, Some("no filesystem found")),
            (
                "lvm/rl/off",
                0,
                None,
                false,
                Some("logical volume could not be activated")
            ),
            ("lvm/rl/root", 0, Some("xfs"), true, None),
            (
                "lvm/rl/swap",
                0,
                Some("swap"),
                false,
                Some("swap — not supported")
            ),
            ("lvm/rl/thin1", 0, Some("ext4"), true, None),
        ]
    );
    let efi = &vols[0];
    assert_eq!(efi.kind, VolumeKind::Partition);
    assert_eq!(
        efi.partition,
        Some(Partition {
            table: PartitionTable::Gpt,
            number: 1,
            type_name: "EFI System".into(),
            uuid: Some("68b2905b-df3e-4fb3-80fa-49d1e773aa33".into()),
        })
    );
    assert_eq!(efi.uuid.as_deref(), Some("7B77-95E7"));
    let data = &vols[3];
    assert_eq!(data.label.as_deref(), Some("my data \"q\""));
    assert_eq!(data.partition.as_ref().unwrap().table, PartitionTable::Mbr);
    assert_eq!(
        data.partition.as_ref().unwrap().uuid,
        None,
        "MBR has no partition uuid"
    );
    assert_eq!(
        vols[4].partition.as_ref().unwrap().type_name,
        "HPFS/NTFS/exFAT (0x07)"
    );
    assert_eq!(vols[6].kind, VolumeKind::Disk);
    assert_eq!(vols[9].kind, VolumeKind::LogicalVolume);
    assert_eq!(vols[9].size, 8_585_740_288);
    assert_eq!(found[9].dev, "/dev/mapper/rl-root");
    assert_eq!(found[1].dev, "/dev/vda2");
    // LVM 레이블 빈 값은 없음으로
    assert_eq!(vols[5].label, None);
}

#[test]
fn odd_lvm_names_fall_back_to_uuids() {
    let lv = |vg: &str, name: &str, vg_uuid: &str, lv_uuid: &str| LvProbe {
        report: LvReport {
            vg_name: vg.into(),
            lv_name: name.into(),
            vg_uuid: vg_uuid.into(),
            lv_uuid: lv_uuid.into(),
            lv_size: "1".into(),
            lv_attr: "-wi-a-----".into(),
            lv_dm_path: "/dev/mapper/x".into(),
        },
        blkid: Default::default(),
    };
    let found = build_volumes(
        &[],
        &[],
        &[
            lv("vg", "bad name", "VgU", "LvU"),
            lv("vg", "x/y", "bad uuid", "z"),
            lv("vg", "ok", "u", "v"),
            lv("vg", "ok", "u", "v"),
        ],
    );
    let ids: Vec<&str> = found.iter().map(|f| f.volume.id.as_str()).collect();
    assert_eq!(ids, ["lvm/VgU/LvU", "lvm/vg/ok"]);
}

#[test]
fn empty_partition_table_and_long_labels() {
    let d = DiskProbe {
        index: 0,
        disk: block("/dev/vda", 1, "PTTYPE=gpt\n"),
        partitions: vec![],
    };
    let long = format!("TYPE=ext4\nLABEL={}\n", "é".repeat(300));
    let e = DiskProbe {
        index: 1,
        disk: block("/dev/vdb", 1, &long),
        partitions: vec![],
    };
    let found = build_volumes(&[d, e], &[], &[]);
    assert_eq!(
        found[0].volume.note.as_deref(),
        Some("empty partition table")
    );
    assert!(!found[0].volume.mountable);
    let label = found[1].volume.label.as_ref().unwrap();
    assert!(label.len() <= MAX_LABEL && label.chars().all(|c| c == 'é'));
}

#[test]
fn volume_count_is_capped() {
    let disks: Vec<DiskProbe> = (0..300)
        .map(|i| DiskProbe {
            index: i,
            disk: block(&format!("/dev/vd{i}"), 1, "TYPE=ext4\n"),
            partitions: vec![],
        })
        .collect();
    assert_eq!(build_volumes(&disks, &[], &[]).len(), MAX_VOLUMES);
}
