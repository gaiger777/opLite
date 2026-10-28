//! `/proc/self/mountinfo` 해석. 마운트 지점을 `stat`하지 않고(멈춘 NFS에서는 끝나지 않는다)
//! 마운트됐는지 판단한다.

use std::collections::BTreeSet;
use std::path::PathBuf;

/// 이 프로세스의 마운트 목록 파일. Linux에만 있다.
pub const MOUNTINFO: &str = "/proc/self/mountinfo";

/// mountinfo에서 NFS(`nfs`, `nfs4`) 마운트 지점들
pub fn nfs_mount_points(mountinfo: &str) -> BTreeSet<PathBuf> {
    mountinfo
        .lines()
        .filter_map(|line| {
            // `id parent major:minor root 마운트지점 옵션 [선택 필드...] - 종류 원본 슈퍼블록옵션`
            let (left, right) = line.split_once(" - ")?;
            let point = left.split(' ').nth(4)?;
            let fstype = right.split(' ').next()?;
            matches!(fstype, "nfs" | "nfs4").then(|| PathBuf::from(unescape(point)))
        })
        .collect()
}

/// mountinfo가 `\040`처럼 8진수로 적은 공백·탭·줄바꿈·역슬래시를 되돌린다.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let code = rest.get(i + 1..i + 4);
        match code.and_then(|c| u8::from_str_radix(c, 8).ok()) {
            Some(b) => {
                out.push(b as char);
                rest = &rest[i + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_nfs_mount_points_in_mountinfo() {
        let info = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 22 0:45 / /var/lib/opl/pools/a rw,relatime shared:20 - nfs4 nas:/srv/vm rw,vers=4.2
41 22 0:46 / /var/lib/opl/pools/b rw,relatime - nfs [fd00::5]:/srv/vm rw,vers=3
42 22 0:47 / /mnt/with\\040space rw - nfs4 nas:/x rw
43 22 0:48 / /mnt/tmp rw - tmpfs tmpfs rw
";
        let points = nfs_mount_points(info);
        assert_eq!(
            points,
            BTreeSet::from([
                PathBuf::from("/var/lib/opl/pools/a"),
                PathBuf::from("/var/lib/opl/pools/b"),
                PathBuf::from("/mnt/with space"),
            ])
        );
        assert_eq!(unescape("a\\134b\\011c\\x"), "a\\b\tc\\x");
    }
}
