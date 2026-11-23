//! 파일시스템 종류별 마운트 옵션(설계 §9.4). 늘 `ro,nodev,nosuid,noexec`이고, 디스크는 버리는 덮개 위라 저널 재생이
//! 덮개에만 쓴다. 재생이 실패하면 재생을 건너뛰는 옵션으로 한 번 더(성공하면 [`RETRY_NOTE`]).

/// 저널을 건너뛰고 붙었을 때 볼륨에 붙는 덧붙임
pub const RETRY_NOTE: &str = "journal not replayed — recent changes may be missing";

/// 마운트 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountPlan {
    /// `mount(2)`의 파일시스템 종류
    pub fstype: &'static str,
    /// 데이터(옵션) 문자열
    pub data: &'static str,
    /// 첫 시도가 실패하면 이 옵션으로 한 번 더
    pub retry: Option<&'static str>,
}

/// blkid `TYPE`의 분류
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsClass {
    Mount(MountPlan),
    /// LVM 물리 볼륨(논리 볼륨이 따로 볼륨으로 보임)
    LvmMember,
    /// 암호화(LUKS·BitLocker) — 지원 안 함
    Encrypted(&'static str),
    /// 알지만 붙이지 않는 것(swap·ZFS·mdraid·ReFS …)과 모르는 것
    Unsupported,
}

/// blkid `TYPE` → 분류
pub fn classify(fs: &str) -> FsClass {
    let plan = |fstype, data, retry| {
        FsClass::Mount(MountPlan {
            fstype,
            data,
            retry,
        })
    };
    match fs {
        // ext2·ext3도 ext4 드라이버로 붙인다(Debian 커널 CONFIG_EXT4_USE_FOR_EXT2).
        "ext2" | "ext3" | "ext4" => plan("ext4", "", Some("noload")),
        "xfs" => plan("xfs", "nouuid", Some("norecovery,nouuid")),
        // 최상위 하위 볼륨(5)을 붙여 모든 하위 볼륨이 디렉터리로 보인다.
        "btrfs" => plan("btrfs", "subvolid=5", Some("rescue=nologreplay,subvolid=5")),
        "ntfs" => plan("ntfs3", "iocharset=utf8", None),
        "vfat" => plan("vfat", "utf8,shortname=mixed", None),
        "exfat" => plan("exfat", "iocharset=utf8", None),
        "LVM2_member" => FsClass::LvmMember,
        "crypto_LUKS" => FsClass::Encrypted("LUKS"),
        "BitLocker" => FsClass::Encrypted("BitLocker"),
        _ => FsClass::Unsupported,
    }
}

/// 볼륨의 (붙일 수 있는지, 덧붙임)
pub fn mountability(fs: Option<&str>) -> (bool, Option<String>) {
    let Some(fs) = fs else {
        return (false, Some("no filesystem found".to_string()));
    };
    match classify(fs) {
        FsClass::Mount(_) => (true, None),
        FsClass::LvmMember => (false, Some("LVM physical volume".to_string())),
        FsClass::Encrypted(what) => (false, Some(format!("encrypted ({what}) — not supported"))),
        FsClass::Unsupported => (false, Some(format!("{fs} — not supported"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        let cases: &[(&str, &str, &str, Option<&str>)] = &[
            ("ext2", "ext4", "", Some("noload")),
            ("ext3", "ext4", "", Some("noload")),
            ("ext4", "ext4", "", Some("noload")),
            ("xfs", "xfs", "nouuid", Some("norecovery,nouuid")),
            (
                "btrfs",
                "btrfs",
                "subvolid=5",
                Some("rescue=nologreplay,subvolid=5"),
            ),
            ("ntfs", "ntfs3", "iocharset=utf8", None),
            ("vfat", "vfat", "utf8,shortname=mixed", None),
            ("exfat", "exfat", "iocharset=utf8", None),
        ];
        for (blkid, fstype, data, retry) in cases {
            assert_eq!(
                classify(blkid),
                FsClass::Mount(MountPlan {
                    fstype,
                    data,
                    retry: *retry
                }),
                "{blkid}"
            );
            assert_eq!(mountability(Some(blkid)), (true, None));
        }
    }

    #[test]
    fn not_mountable() {
        assert_eq!(
            mountability(Some("LVM2_member")),
            (false, Some("LVM physical volume".into()))
        );
        assert_eq!(
            mountability(Some("crypto_LUKS")),
            (false, Some("encrypted (LUKS) — not supported".into()))
        );
        assert_eq!(
            mountability(Some("BitLocker")),
            (false, Some("encrypted (BitLocker) — not supported".into()))
        );
        for fs in ["swap", "zfs_member", "linux_raid_member", "ReFS", "hfsplus"] {
            assert_eq!(
                mountability(Some(fs)),
                (false, Some(format!("{fs} — not supported")))
            );
        }
        assert_eq!(
            mountability(None),
            (false, Some("no filesystem found".into()))
        );
    }
}
