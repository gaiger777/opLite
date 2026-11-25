//! 노드의 디스크를 OSD로 쓸 수 있는지 가른다. 입력은 `lsblk`·`pvs`·`ceph-volume lvm list`의 출력과 이
//! 노드의 Orbivisor LVM 풀 VG다. 시스템 디스크·마운트·스왑·ZFS·RAID·풀 VG·이 Ceph의 OSD는 지우기로도 쓰지
//! 않는다([`DiskState::InUse`]).

use std::collections::BTreeSet;

use orbivisor_types::ceph::{DiskState, MIN_OSD_BYTES, NodeDisk};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::json::VolumeOsd;

/// `lsblk`에 줄 열(util-linux 2.39)
pub const LSBLK_COLUMNS: &str =
    "NAME,KNAME,TYPE,SIZE,RO,RM,ROTA,MODEL,SERIAL,WWN,FSTYPE,PTTYPE,MOUNTPOINTS,PKNAME";

/// `lsblk --json --bytes --paths --output {LSBLK_COLUMNS}`
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct LsblkReport {
    pub blockdevices: Vec<BlockDevice>,
}

/// 블록 장치 하나와 그 아래(파티션·LV·dm)
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct BlockDevice {
    /// `--paths`라 `/dev/sdb`
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(deserialize_with = "number")]
    pub size: u64,
    #[serde(deserialize_with = "flag")]
    pub ro: bool,
    #[serde(deserialize_with = "flag")]
    pub rm: bool,
    #[serde(deserialize_with = "flag")]
    pub rota: bool,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub fstype: Option<String>,
    pub pttype: Option<String>,
    /// 마운트되지 않았으면 `[null]`
    pub mountpoints: Vec<Option<String>>,
    /// 옛 lsblk(`MOUNTPOINT`)
    pub mountpoint: Option<String>,
    pub children: Vec<BlockDevice>,
}

/// lsblk 판에 따라 참거짓이 `true`·`"1"`·`1`로 온다.
fn flag<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::Number(n) => n.as_u64().is_some_and(|n| n != 0),
        Value::String(s) => s == "1" || s == "true",
        _ => false,
    })
}

/// `--bytes`의 크기가 수나 문자열로 온다.
fn number<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().unwrap_or(0),
        Value::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    })
}

impl BlockDevice {
    /// 자기와 그 아래 모든 장치
    fn all(&self) -> Vec<&BlockDevice> {
        let mut all = vec![self];
        for child in &self.children {
            all.extend(child.all());
        }
        all
    }

    fn mounts(&self) -> impl Iterator<Item = &str> {
        self.mountpoints
            .iter()
            .flatten()
            .chain(&self.mountpoint)
            .map(String::as_str)
            .filter(|m| !m.is_empty())
    }
}

/// lsblk 출력
pub fn parse_lsblk(out: &[u8]) -> Result<LsblkReport, String> {
    serde_json::from_slice(out).map_err(|e| format!("unexpected lsblk output: {e}"))
}

/// LVM PV 하나(`pvs`). VG가 없는 PV는 `vg_name`이 비어 있다.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct PvInfo {
    pub pv_name: String,
    pub vg_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PvsJson {
    report: Vec<PvsEntryJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PvsEntryJson {
    pv: Vec<PvInfo>,
}

/// `pvs --reportformat json -o pv_name,vg_name`의 모든 PV
pub fn parse_pvs(out: &[u8]) -> Result<Vec<PvInfo>, String> {
    let report: PvsJson =
        serde_json::from_slice(out).map_err(|e| format!("unexpected pvs output: {e}"))?;
    Ok(report.report.into_iter().flat_map(|r| r.pv).collect())
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LvsJson {
    report: Vec<LvsEntryJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LvsEntryJson {
    lv: Vec<LvJson>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LvJson {
    vg_name: String,
    lv_attr: String,
}

/// `lvs --reportformat json -o lv_name,vg_name,lv_attr`에서 열린 LV(`lv_attr`의 여섯째 글자 `o` — 돌고 있는 OSD나
/// 다른 장치가 쓰는 중)가 있는 VG(이름 순)
pub fn parse_open_vgs(out: &[u8]) -> Result<Vec<String>, String> {
    let report: LvsJson =
        serde_json::from_slice(out).map_err(|e| format!("unexpected lvs output: {e}"))?;
    let vgs: BTreeSet<String> = report
        .report
        .into_iter()
        .flat_map(|r| r.lv)
        .filter(|lv| lv.lv_attr.chars().nth(5) == Some('o'))
        .map(|lv| lv.vg_name)
        .collect();
    Ok(vgs.into_iter().collect())
}

/// 디스크를 가르는 데 쓰는 이 노드의 사정
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiskContext {
    /// 이 노드가 쓰는 Orbivisor LVM·공유 LVM 풀의 (VG, 풀 이름)
    pub pool_vgs: Vec<(String, String)>,
    /// 모든 PV
    pub pv_vgs: Vec<PvInfo>,
    /// 열린 LV가 있는 VG([`parse_open_vgs`]). 그 PV의 디스크는 지우지 않는다(남의 OSD가 돌고 있을 수 있다).
    pub open_vgs: Vec<String>,
    /// 이 Ceph 클러스터(우리 fsid)의 OSD
    pub ceph_osds: Vec<VolumeOsd>,
    /// 루프 장치도 디스크로 보인다(시험용).
    pub allow_loop: bool,
    /// `/dev/disk/by-id`의 (링크 경로, 가리키는 장치 경로)
    pub by_id: Vec<(String, String)>,
}

/// 디스크 `path`의 by-id 링크: `wwn-` > `nvme-eui.` > 그 밖(이름 순) 첫 것. LVM·dm·md 이름 링크는
/// 디스크를 지우거나 바꾸면 사라지므로 고르지 않는다.
pub fn pick_by_id(path: &str, links: &[(String, String)]) -> Option<String> {
    let mut found: Vec<&str> = links
        .iter()
        .filter(|(_, target)| target == path)
        .map(|(link, _)| link.as_str())
        .filter(|link| {
            let name = link.rsplit('/').next().unwrap_or(link);
            !["lvm-pv-uuid-", "dm-", "md-"]
                .iter()
                .any(|p| name.starts_with(p))
        })
        .collect();
    let rank = |link: &str| {
        let name = link.rsplit('/').next().unwrap_or(link);
        if name.starts_with("wwn-") {
            0
        } else if name.starts_with("nvme-eui.") {
            1
        } else {
            2
        }
    };
    found.sort_by_key(|l| (rank(l), *l));
    found.first().map(|l| l.to_string())
}

/// 시스템 디스크로 보는 마운트
const SYSTEM_MOUNTS: [&str; 3] = ["/", "/boot", "/boot/efi"];

/// 지우기로도 쓸 수 없는 이유
fn in_use(disk: &BlockDevice, ctx: &DiskContext) -> Option<String> {
    if disk.ro {
        return Some("read-only".into());
    }
    if disk.rm {
        return Some("removable".into());
    }
    if disk.size < MIN_OSD_BYTES {
        return Some("smaller than 5 GiB".into());
    }
    let all = disk.all();
    let mounts: Vec<&str> = all.iter().flat_map(|d| d.mounts()).collect();
    if let Some(m) = SYSTEM_MOUNTS.iter().find(|m| mounts.contains(m)) {
        return Some(format!("system disk ({m} is on it)"));
    }
    if let Some(m) = mounts.iter().find(|m| **m != "[SWAP]") {
        return Some(format!("mounted at {m}"));
    }
    if !mounts.is_empty() {
        return Some("used as swap".into());
    }
    let fstypes: BTreeSet<&str> = all.iter().filter_map(|d| d.fstype.as_deref()).collect();
    if fstypes.contains("zfs_member") {
        return Some("ZFS pool member".into());
    }
    if fstypes.contains("linux_raid_member") {
        return Some("software RAID member".into());
    }
    let paths: BTreeSet<&str> = all.iter().map(|d| d.name.as_str()).collect();
    if let Some(osd) = ctx
        .ceph_osds
        .iter()
        .find(|o| o.devices.iter().any(|d| paths.contains(d.as_str())))
    {
        return Some(format!("osd.{} of this Ceph cluster", osd.id));
    }
    for pv in ctx
        .pv_vgs
        .iter()
        .filter(|p| paths.contains(p.pv_name.as_str()))
    {
        if let Some((vg, pool)) = ctx.pool_vgs.iter().find(|(vg, _)| *vg == pv.vg_name) {
            return Some(format!(
                "physical volume of VG {vg} used by storage pool {pool}"
            ));
        }
        if !pv.vg_name.is_empty() && ctx.open_vgs.contains(&pv.vg_name) {
            return Some("in use by a running Ceph OSD or another device".into());
        }
        let elsewhere = ctx
            .pv_vgs
            .iter()
            .any(|other| other.vg_name == pv.vg_name && !paths.contains(other.pv_name.as_str()));
        // 남의 Ceph VG도 다른 디스크에 걸쳐 있으면 지우지 않는다(이 Ceph의 OSD는 위에서 이미 걸렀다).
        if !pv.vg_name.is_empty() && elsewhere {
            return Some(format!("VG {} spans other disks", pv.vg_name));
        }
    }
    None
}

/// 지우면 쓸 수 있는 이유들(없으면 빈 디스크)
fn wipe_reasons(disk: &BlockDevice, ctx: &DiskContext) -> Vec<String> {
    let mut reasons = Vec::new();
    let mut add = |r: String| {
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    };
    if let Some(pt) = &disk.pttype {
        add(format!("{pt} partition table"));
    }
    let all = disk.all();
    let parts = disk.children.iter().filter(|c| c.kind == "part").count();
    if parts > 0 {
        add(format!("{parts} partition(s)"));
    }
    let paths: BTreeSet<&str> = all.iter().map(|d| d.name.as_str()).collect();
    for pv in ctx
        .pv_vgs
        .iter()
        .filter(|p| paths.contains(p.pv_name.as_str()))
    {
        add(match pv.vg_name.as_str() {
            "" => "LVM physical volume".into(),
            vg if vg.starts_with("ceph-") => "left over from another Ceph cluster".into(),
            vg => format!("LVM physical volume of VG {vg}"),
        });
    }
    for dev in &all {
        match dev.fstype.as_deref() {
            None | Some("") => {}
            Some("ceph_bluestore") => add("left over from another Ceph cluster".into()),
            // `pvs`가 보지 못한 PV(위에서 VG와 함께 적지 못한 것)
            Some("LVM2_member") => {
                if !ctx.pv_vgs.iter().any(|p| p.pv_name == dev.name) {
                    add("LVM physical volume".into());
                }
            }
            Some(fs) if dev.name == disk.name => add(format!("{fs} filesystem")),
            Some(fs) => add(format!("{fs} filesystem on {}", dev.name)),
        }
    }
    reasons
}

/// 최상위 디스크(`TYPE disk`, `allow_loop`면 `loop`도)마다 OSD로 쓸 수 있는지
pub fn classify(lsblk: &LsblkReport, ctx: &DiskContext) -> Vec<NodeDisk> {
    lsblk
        .blockdevices
        .iter()
        .filter(|d| d.kind == "disk" || (ctx.allow_loop && d.kind == "loop"))
        .map(|disk| {
            let state = match in_use(disk, ctx) {
                Some(reason) => DiskState::InUse { reason },
                None => match wipe_reasons(disk, ctx) {
                    reasons if reasons.is_empty() => DiskState::Available,
                    reasons => DiskState::Wipeable { reasons },
                },
            };
            let text = |s: &Option<String>| {
                s.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            NodeDisk {
                path: disk.name.clone(),
                by_id: pick_by_id(&disk.name, &ctx.by_id),
                size_bytes: disk.size,
                rotational: disk.rota,
                model: text(&disk.model),
                serial: text(&disk.serial),
                state,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ceph::json::parse_volume_list;

    const ROOT: &[u8] = include_bytes!("fixtures/lsblk-root.json");
    const LVM: &[u8] = include_bytes!("fixtures/lsblk-lvm.json");
    const LOOP: &[u8] = include_bytes!("fixtures/lsblk-loop.json");
    const PVS: &[u8] = include_bytes!("fixtures/pvs.json");
    const VOLUME_LIST: &[u8] = include_bytes!("fixtures/ceph-volume-list.json");
    const FSID: &str = "18796e2f-112c-44cf-9d48-09f825f0a14b";

    fn ctx() -> DiskContext {
        DiskContext {
            pool_vgs: vec![("vg0".into(), "fast".into())],
            pv_vgs: parse_pvs(PVS).unwrap(),
            open_vgs: vec![],
            ceph_osds: parse_volume_list(VOLUME_LIST)
                .unwrap()
                .into_iter()
                .filter(|o| o.cluster_fsid == FSID)
                .collect(),
            allow_loop: false,
            by_id: vec![],
        }
    }

    fn states(out: &[u8], ctx: &DiskContext) -> Vec<(String, DiskState)> {
        classify(&parse_lsblk(out).unwrap(), ctx)
            .into_iter()
            .map(|d| (d.path, d.state))
            .collect()
    }

    fn in_use(reason: &str) -> DiskState {
        DiskState::InUse {
            reason: reason.into(),
        }
    }

    fn wipeable(reasons: &[&str]) -> DiskState {
        DiskState::Wipeable {
            reasons: reasons.iter().map(|r| r.to_string()).collect(),
        }
    }

    /// 루트 디스크(`/`·`/boot/efi`·스왑이 파티션·LV에), 빈 디스크, 마운트되지 않은 GPT+ext4, 마운트된
    /// 디스크, NVMe, 읽기 전용·이동식·4 GiB. CD-ROM(`rom`)은 디스크가 아니다.
    #[test]
    fn classifies_system_empty_mounted_and_small_disks() {
        let ctx = DiskContext {
            by_id: vec![
                (
                    "/dev/disk/by-id/ata-QEMU_HARDDISK_drive-scsi1".into(),
                    "/dev/sdb".into(),
                ),
                (
                    "/dev/disk/by-id/wwn-0x5000c500a1b2c3d5".into(),
                    "/dev/sdb".into(),
                ),
                (
                    "/dev/disk/by-id/nvme-Samsung_SSD_980_S64DNF0R123456".into(),
                    "/dev/nvme0n1".into(),
                ),
                (
                    "/dev/disk/by-id/nvme-eui.002538b111b22c33".into(),
                    "/dev/nvme0n1".into(),
                ),
                (
                    "/dev/disk/by-id/ata-ST2000DM008_ZFL0ABCD-part1".into(),
                    "/dev/sdc1".into(),
                ),
            ],
            ..ctx()
        };
        assert_eq!(
            states(ROOT, &ctx),
            [
                ("/dev/sda".into(), in_use("system disk (/ is on it)")),
                ("/dev/sdb".into(), DiskState::Available),
                (
                    "/dev/sdc".into(),
                    wipeable(&[
                        "gpt partition table",
                        "1 partition(s)",
                        "ext4 filesystem on /dev/sdc1"
                    ])
                ),
                ("/dev/sdd".into(), in_use("mounted at /srv")),
                ("/dev/nvme0n1".into(), DiskState::Available),
                ("/dev/sde".into(), in_use("read-only")),
                ("/dev/sdf".into(), in_use("removable")),
                ("/dev/sdg".into(), in_use("smaller than 5 GiB")),
            ]
        );
        let disks = classify(&parse_lsblk(ROOT).unwrap(), &ctx);
        let sda = &disks[0];
        // 모델 끝의 빈칸은 뺀다.
        assert_eq!(sda.model.as_deref(), Some("QEMU HARDDISK"));
        assert_eq!(sda.serial.as_deref(), Some("drive-scsi0"));
        assert_eq!((sda.size_bytes, sda.rotational), (64 << 30, true));
        assert_eq!(
            disks[1].by_id.as_deref(),
            Some("/dev/disk/by-id/wwn-0x5000c500a1b2c3d5")
        );
        assert_eq!(disks[2].by_id, None);
        assert_eq!(
            disks[4].by_id.as_deref(),
            Some("/dev/disk/by-id/nvme-eui.002538b111b22c33")
        );
        assert!(!disks[4].rotational);
        assert_eq!(disks[7].model, None);
    }

    /// PV(풀 VG / 다른 VG / 두 디스크에 걸친 VG), ZFS, 남의 Ceph, 이 Ceph의 OSD, RAID, 스왑 디스크, VG 없는
    /// PV, bluestore 서명만 남은 디스크
    #[test]
    fn classifies_lvm_zfs_raid_and_ceph_disks() {
        assert_eq!(
            states(LVM, &ctx()),
            [
                (
                    "/dev/sdh".into(),
                    in_use("physical volume of VG vg0 used by storage pool fast")
                ),
                (
                    "/dev/sdi".into(),
                    wipeable(&[
                        "LVM physical volume of VG data",
                        "xfs filesystem on /dev/mapper/data-archive"
                    ])
                ),
                ("/dev/sdj".into(), in_use("VG wide spans other disks")),
                ("/dev/sdk".into(), in_use("VG wide spans other disks")),
                ("/dev/sdl".into(), in_use("ZFS pool member")),
                (
                    "/dev/sdm".into(),
                    wipeable(&["left over from another Ceph cluster"])
                ),
                ("/dev/sdn".into(), in_use("osd.0 of this Ceph cluster")),
                ("/dev/sdo".into(), in_use("software RAID member")),
                ("/dev/sdp".into(), in_use("used as swap")),
                ("/dev/sdq".into(), wipeable(&["LVM physical volume"])),
                (
                    "/dev/sdr".into(),
                    wipeable(&["left over from another Ceph cluster"])
                ),
            ]
        );
    }

    /// 다른 OSD 목록이면(우리 fsid가 아님) 그 OSD 디스크도 남의 Ceph의 흔적이다. 풀이 아니면 VG는 지울 수
    /// 있다.
    #[test]
    fn foreign_osds_and_non_pool_vgs_are_wipeable() {
        let ctx = DiskContext {
            pool_vgs: vec![],
            ceph_osds: vec![],
            ..ctx()
        };
        let all = states(LVM, &ctx);
        assert_eq!(
            all[0],
            (
                "/dev/sdh".into(),
                wipeable(&["LVM physical volume of VG vg0"])
            )
        );
        assert_eq!(
            all[6],
            (
                "/dev/sdn".into(),
                wipeable(&["left over from another Ceph cluster"])
            )
        );
    }

    /// 다른 디스크에 걸친 VG는 이름이 `ceph-`로 시작해도(남의 Ceph) 쓰는 중이다. 이 Ceph의 OSD는 그 이유가 먼저다.
    #[test]
    fn a_ceph_volume_group_spanning_disks_is_in_use() {
        let mut ctx = ctx();
        for (pv, vg) in [
            ("/dev/sdq", "ceph-9a1e2b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ("/dev/sdr", "ceph-4236eeea-189f-4909-b2ff-03683a9a0b9c"),
        ] {
            ctx.pv_vgs.retain(|p| p.pv_name != pv);
            ctx.pv_vgs.push(PvInfo {
                pv_name: pv.into(),
                vg_name: vg.into(),
            });
        }
        let all = states(LVM, &ctx);
        let spans = in_use("VG ceph-9a1e2b3c-4d5e-4f60-8a71-b2c3d4e5f607 spans other disks");
        assert_eq!(all[5], ("/dev/sdm".into(), spans.clone()));
        assert_eq!(all[9], ("/dev/sdq".into(), spans));
        assert_eq!(
            all[6],
            ("/dev/sdn".into(), in_use("osd.0 of this Ceph cluster"))
        );
    }

    /// 열린 LV가 있는 VG(돌고 있는 남의 OSD — cephadm 등)의 디스크는 지우기로도 쓰지 않는다. 우리 OSD·풀 VG는
    /// 그 이유가 먼저다.
    #[test]
    fn disks_of_open_volume_groups_are_in_use() {
        let lvs = br#"{"report":[{"lv":[
            {"lv_name":"osd-block-5d1c","vg_name":"ceph-9a1e2b3c-4d5e-4f60-8a71-b2c3d4e5f607","lv_attr":"-wi-ao----"},
            {"lv_name":"osd-block-0851","vg_name":"ceph-4236eeea-189f-4909-b2ff-03683a9a0b9c","lv_attr":"-wi-ao----"},
            {"lv_name":"thin","vg_name":"vg0","lv_attr":"twi-aotz--"},
            {"lv_name":"archive","vg_name":"data","lv_attr":"-wi-a-----"}]}],"log":[]}"#;
        let open = parse_open_vgs(lvs).unwrap();
        assert_eq!(
            open,
            [
                "ceph-4236eeea-189f-4909-b2ff-03683a9a0b9c",
                "ceph-9a1e2b3c-4d5e-4f60-8a71-b2c3d4e5f607",
                "vg0"
            ]
        );
        let ctx = DiskContext {
            open_vgs: open,
            ..ctx()
        };
        let all = states(LVM, &ctx);
        assert_eq!(
            all[5],
            (
                "/dev/sdm".into(),
                in_use("in use by a running Ceph OSD or another device")
            )
        );
        assert_eq!(
            all[6],
            ("/dev/sdn".into(), in_use("osd.0 of this Ceph cluster"))
        );
        assert_eq!(
            all[0],
            (
                "/dev/sdh".into(),
                in_use("physical volume of VG vg0 used by storage pool fast")
            )
        );
        // 닫힌 LV뿐인 VG는 지울 수 있다.
        assert_eq!(
            all[1],
            (
                "/dev/sdi".into(),
                wipeable(&[
                    "LVM physical volume of VG data",
                    "xfs filesystem on /dev/mapper/data-archive"
                ])
            )
        );
        assert!(parse_open_vgs(b"x").is_err());
    }

    /// 루프 장치는 `allow_loop`일 때만 보인다. 마운트된 작은 루프(snap)는 쓸 수 없다.
    #[test]
    fn loop_devices_only_when_allowed() {
        assert!(states(LOOP, &ctx()).is_empty());
        let ctx = DiskContext {
            allow_loop: true,
            ..ctx()
        };
        assert_eq!(
            states(LOOP, &ctx),
            [
                ("/dev/loop0".into(), in_use("smaller than 5 GiB")),
                ("/dev/loop2".into(), DiskState::Available),
                ("/dev/loop3".into(), wipeable(&["ext4 filesystem"])),
            ]
        );
    }

    /// 옛 lsblk의 `"1"`·`"0"` 참거짓과 문자열 크기, `mountpoint` 한 값도 읽는다.
    #[test]
    fn reads_old_lsblk_shapes() {
        let out = br#"{"blockdevices":[
            {"name":"/dev/sdx","type":"disk","size":"10737418240","ro":"0","rm":"0","rota":"1",
             "mountpoint":"/data"},
            {"name":"/dev/sdy","type":"disk","size":10737418240,"ro":1,"rm":0,"rota":0}]}"#;
        let report = parse_lsblk(out).unwrap();
        assert_eq!(report.blockdevices[0].size, 10 << 30);
        assert!(report.blockdevices[0].rota && !report.blockdevices[1].rota);
        assert_eq!(
            states(out, &DiskContext::default()),
            [
                ("/dev/sdx".into(), in_use("mounted at /data")),
                ("/dev/sdy".into(), in_use("read-only")),
            ]
        );
        assert!(parse_lsblk(b"lsblk: unknown column").is_err());
        assert!(parse_pvs(b"x").is_err());
    }

    #[test]
    fn picks_by_id_links() {
        let links = |names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .map(|n| (format!("/dev/disk/by-id/{n}"), "/dev/sdb".to_string()))
                .collect()
        };
        let pick = |names: &[&str]| pick_by_id("/dev/sdb", &links(names));
        assert_eq!(
            pick(&["scsi-0QEMU", "wwn-0x1", "ata-QEMU"]).as_deref(),
            Some("/dev/disk/by-id/wwn-0x1")
        );
        assert_eq!(
            pick(&["nvme-Samsung_X", "nvme-eui.0025"]).as_deref(),
            Some("/dev/disk/by-id/nvme-eui.0025")
        );
        assert_eq!(
            pick(&["scsi-0QEMU", "lvm-pv-uuid-abc", "ata-QEMU"]).as_deref(),
            Some("/dev/disk/by-id/ata-QEMU")
        );
        assert_eq!(pick(&["lvm-pv-uuid-abc", "dm-name-x", "md-uuid-y"]), None);
        assert_eq!(pick(&[]), None);
        assert_eq!(pick_by_id("/dev/sdc", &links(&["wwn-0x1"])), None);
    }
}
