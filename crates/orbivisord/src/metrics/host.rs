//! 호스트 값 읽기. 측정기가 5초마다 부르는 누적 카운터와 지금 값이다(비율은 측정기가 계산한다).
//!
//! Linux는 `/proc`·`/sys`를 읽고(파서는 글을 받는 순수 함수), macOS는 libc의 Mach·`proc_pidinfo`·
//! `getifaddrs`를 부른다. 그 밖의 OS는 모두 `None`이다.

use std::path::Path;
#[cfg(any(target_os = "linux", test))]
use std::path::PathBuf;
use std::sync::Arc;

/// CPU 누적 시간(단위는 구현마다 다르고 비율만 쓴다)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// 프로세스 하나의 누적 CPU 시간과 RSS
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    /// 사용자 + 커널 CPU 시간(나노초)
    pub cpu_ns: u64,
    pub rss_mib: u64,
}

/// 호스트 값을 읽는다. 읽지 못하면 `None`이다.
pub trait HostProbe: Send + Sync {
    /// 모든 코어를 합한 CPU 시간
    fn cpu_times(&self) -> Option<CpuTimes>;
    /// (쓰는 MiB, 전체 MiB)
    fn memory(&self) -> Option<(u64, u64)>;
    /// 실제 디스크에서 (읽은, 쓴) 누적 바이트
    fn disk_bytes(&self) -> Option<(u64, u64)>;
    /// 실제 NIC의 (받은, 보낸) 누적 바이트
    fn nic_bytes(&self) -> Option<(u64, u64)>;
    fn process(&self, pid: u32) -> Option<ProcStat>;
    /// tap 장치의 호스트 쪽 (받은, 보낸) 누적 바이트. 게스트 기준으로는 거꾸로다.
    fn tap_bytes(&self, name: &str) -> Option<(u64, u64)>;
    /// `path`가 있는 파일 시스템의 (쓰는 MiB, 전체 MiB)
    fn fs(&self, path: &Path) -> Option<(u64, u64)>;
}

/// 이 OS의 [`HostProbe`]
pub fn native() -> Arc<dyn HostProbe> {
    #[cfg(target_os = "linux")]
    let probe = Arc::new(LinuxProbe::default());
    #[cfg(target_os = "macos")]
    let probe = Arc::new(MacProbe::default());
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let probe = Arc::new(NoProbe);
    probe
}

/// `statvfs`로 (쓰는 MiB, 전체 MiB)
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn statvfs_mib(path: &Path) -> Option<(u64, u64)> {
    let st = rustix::fs::statvfs(path).ok()?;
    let total = st.f_blocks * st.f_frsize;
    let free = st.f_bfree * st.f_frsize;
    Some((total.saturating_sub(free) >> 20, total >> 20))
}

/// `/proc/stat` 첫 줄(`cpu  user nice system idle iowait irq softirq steal …`). guest는 user에 이미
/// 들어 있어 앞의 여덟 값만 더하고, 바쁜 시간은 idle과 iowait를 뺀 것이다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_proc_stat(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() < 5 {
        return None;
    }
    let total: u64 = values.iter().sum();
    Some(CpuTimes {
        busy: total - values[3] - values[4],
        total,
    })
}

/// `/proc/meminfo`의 (MemTotal − MemAvailable, MemTotal) MiB
#[cfg(any(target_os = "linux", test))]
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let total = kib_field(text, "MemTotal:")?;
    let available = kib_field(text, "MemAvailable:")?;
    Some((total.saturating_sub(available) >> 10, total >> 10))
}

/// `Name:   123 kB` 줄의 값(KiB)
#[cfg(any(target_os = "linux", test))]
fn kib_field(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with(name))?;
    line[name.len()..].split_whitespace().next()?.parse().ok()
}

/// 디스크 I/O를 셀 때 빼는 장치. 위에 쌓인 장치(LVM·RAID·ZFS zvol 등)는 아래 디스크와 두 번 세이고,
/// loop·램 디스크·네트워크 블록 장치는 물리 디스크가 아니다.
#[cfg(any(target_os = "linux", test))]
fn is_stacked_or_virtual(name: &str) -> bool {
    ["loop", "ram", "zram", "dm-", "md", "nbd", "zd", "rbd"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// `/proc/diskstats`에서 실제 디스크의 (읽은, 쓴) 바이트. `is_disk(name)`은 `/sys/block/<name>`이 있는지
/// (파티션이 아닌지)다. 섹터는 늘 512바이트다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_diskstats(text: &str, is_disk: impl Fn(&str) -> bool) -> (u64, u64) {
    let mut read = 0;
    let mut written = 0;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // major minor name reads merged sectors_read ms writes merged sectors_written …
        if fields.len() < 10 {
            continue;
        }
        let name = fields[2];
        if is_stacked_or_virtual(name) || !is_disk(name) {
            continue;
        }
        let (Ok(r), Ok(w)) = (fields[5].parse::<u64>(), fields[9].parse::<u64>()) else {
            continue;
        };
        read += r * 512;
        written += w * 512;
    }
    (read, written)
}

/// `/proc/<pid>/stat`의 utime + stime(클록 틱). comm(둘째 값)에 공백·괄호가 들 수 있어 마지막 `)` 뒤를 센다.
#[cfg(any(target_os = "linux", test))]
pub fn parse_pid_stat(text: &str) -> Option<u64> {
    let rest = &text[text.rfind(')')? + 1..];
    // rest의 첫 값이 state(3번째 필드)이고 utime·stime은 14·15번째 필드다.
    let mut fields = rest.split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

/// `/proc/<pid>/status`의 `VmRSS`(MiB)
#[cfg(any(target_os = "linux", test))]
pub fn parse_status_rss(text: &str) -> Option<u64> {
    kib_field(text, "VmRSS:").map(|kib| kib >> 10)
}

/// Linux: `/proc`·`/sys`를 읽는다. 시험은 `root`에 가짜 트리를 둔다.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone)]
pub struct LinuxProbe {
    root: PathBuf,
    clock_ticks: u64,
}

#[cfg(any(target_os = "linux", test))]
impl Default for LinuxProbe {
    fn default() -> Self {
        Self::with_root("/")
    }
}

#[cfg(any(target_os = "linux", test))]
impl LinuxProbe {
    pub fn with_root(root: impl Into<PathBuf>) -> Self {
        // SAFETY: sysconf는 인자만 읽는다.
        let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        Self {
            root: root.into(),
            clock_ticks: u64::try_from(ticks).ok().filter(|&t| t > 0).unwrap_or(100),
        }
    }

    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// `/sys/class/net/<name>/statistics`의 (rx_bytes, tx_bytes)
    fn net_counters(&self, name: &str) -> Option<(u64, u64)> {
        let counter = |c: &str| -> Option<u64> {
            self.read(&format!("sys/class/net/{name}/statistics/{c}"))?
                .trim()
                .parse()
                .ok()
        };
        Some((counter("rx_bytes")?, counter("tx_bytes")?))
    }
}

#[cfg(any(target_os = "linux", test))]
impl HostProbe for LinuxProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        parse_proc_stat(&self.read("proc/stat")?)
    }

    fn memory(&self) -> Option<(u64, u64)> {
        parse_meminfo(&self.read("proc/meminfo")?)
    }

    fn disk_bytes(&self) -> Option<(u64, u64)> {
        let text = self.read("proc/diskstats")?;
        let sys_block = self.root.join("sys/block");
        // `/sys/block`에서는 이름의 `/`가 `!`다(예: cciss!c0d0).
        Some(parse_diskstats(&text, |name| {
            sys_block.join(name.replace('/', "!")).exists()
        }))
    }

    /// `device` 링크가 있는 장치(실제·가상 하드웨어 NIC)만 더한다. 브리지·tap·VXLAN·본드·VLAN은 없다.
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        let dir = std::fs::read_dir(self.root.join("sys/class/net")).ok()?;
        let mut total = (0, 0);
        for entry in dir.flatten() {
            if !entry.path().join("device").exists() {
                continue;
            }
            let Some((rx, tx)) = self.net_counters(&entry.file_name().to_string_lossy()) else {
                continue;
            };
            total.0 += rx;
            total.1 += tx;
        }
        Some(total)
    }

    fn process(&self, pid: u32) -> Option<ProcStat> {
        let ticks = parse_pid_stat(&self.read(&format!("proc/{pid}/stat"))?)?;
        let rss_mib = parse_status_rss(&self.read(&format!("proc/{pid}/status"))?)?;
        Some(ProcStat {
            // tick × 10⁹은 u64를 넘을 수 있다(16 vCPU가 꽉 차면 약 133일).
            cpu_ns: (u128::from(ticks) * 1_000_000_000 / u128::from(self.clock_ticks)) as u64,
            rss_mib,
        })
    }

    fn tap_bytes(&self, name: &str) -> Option<(u64, u64)> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return None;
        }
        self.net_counters(name)
    }

    fn fs(&self, path: &Path) -> Option<(u64, u64)> {
        statvfs_mib(path)
    }
}

/// macOS: Mach 호스트 통계, `proc_pidinfo`, `getifaddrs`. 디스크 I/O와 tap은 없다.
#[cfg(target_os = "macos")]
#[derive(Debug)]
pub struct MacProbe {
    /// `mach_host_self()`는 부를 때마다 포트 참조를 하나 늘리므로 한 번만 얻는다.
    host: libc::mach_port_t,
    /// `proc_pidinfo`의 CPU 시간(Mach 시간 단위) → 나노초
    timebase: (u64, u64),
    /// `hw.memsize`(MiB). 알 수 없으면 0이다.
    memory_total_mib: u64,
}

#[cfg(target_os = "macos")]
impl Default for MacProbe {
    #[allow(deprecated)] // libc는 mach2 크레이트를 권하지만 새 의존성을 더하지 않는다.
    fn default() -> Self {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: 두 함수는 인자만 읽고 `info`에 쓴다.
        let (host, ok) = unsafe { (libc::mach_host_self(), libc::mach_timebase_info(&mut info)) };
        let timebase = if ok == 0 && info.numer > 0 && info.denom > 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        };
        let mut bytes: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: hw.memsize는 u64 하나이고 `len`이 그 크기다.
        let ret = unsafe {
            libc::sysctlbyname(
                c"hw.memsize".as_ptr(),
                &mut bytes as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        Self {
            host,
            timebase,
            memory_total_mib: if ret == 0 { bytes >> 20 } else { 0 },
        }
    }
}

#[cfg(target_os = "macos")]
impl HostProbe for MacProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        let mut cpus: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut count: libc::mach_msg_type_number_t = 0;
        // SAFETY: 성공하면 커널이 코어마다 CPU_STATE_MAX개의 틱을 담은 배열을 우리 주소 공간에 만들어 준다.
        let ret = unsafe {
            libc::host_processor_info(
                self.host,
                libc::PROCESSOR_CPU_LOAD_INFO,
                &mut cpus,
                &mut info,
                &mut count,
            )
        };
        if ret != libc::KERN_SUCCESS || info.is_null() {
            return None;
        }
        // SAFETY: `info`는 `count`개의 integer_t다.
        let ticks = unsafe { std::slice::from_raw_parts(info, count as usize) };
        let states = libc::CPU_STATE_MAX as usize;
        let mut times = CpuTimes { busy: 0, total: 0 };
        for cpu in ticks.chunks_exact(states) {
            let tick = |s: libc::c_int| u64::from(cpu[s as usize] as u32);
            let busy = tick(libc::CPU_STATE_USER)
                + tick(libc::CPU_STATE_SYSTEM)
                + tick(libc::CPU_STATE_NICE);
            times.busy += busy;
            times.total += busy + tick(libc::CPU_STATE_IDLE);
        }
        // SAFETY: 커널이 준 배열을 돌려준다.
        #[allow(deprecated)]
        unsafe {
            libc::vm_deallocate(
                libc::mach_task_self(),
                info as libc::vm_address_t,
                count as libc::vm_size_t * std::mem::size_of::<libc::integer_t>(),
            );
        }
        Some(times)
    }

    /// 쓰는 메모리는 active + wired + 압축 페이지다(활동 모니터의 "사용된 메모리"에 가깝다).
    fn memory(&self) -> Option<(u64, u64)> {
        // SAFETY: vm_statistics64는 정수만 든 C 구조체다.
        let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
        let mut count = libc::HOST_VM_INFO64_COUNT;
        // SAFETY: `count`는 `stats`의 크기(integer_t 개수)이고 커널은 그만큼까지만 쓴다.
        let ret = unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                &mut stats as *mut _ as libc::host_info64_t,
                &mut count,
            )
        };
        if ret != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: sysconf는 인자만 읽는다.
        let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
        let pages = u64::from(stats.active_count)
            + u64::from(stats.wire_count)
            + u64::from(stats.compressor_page_count);
        let total = self.memory_total_mib;
        (total > 0).then(|| ((pages * page) >> 20, total))
    }

    fn disk_bytes(&self) -> Option<(u64, u64)> {
        None
    }

    /// 이름이 `en`으로 시작하는 링크 계층 장치의 `if_data` 카운터. 32비트라 4 GiB마다 돌아간다(측정기가
    /// 줄어든 카운터를 기준으로 다시 잡는다).
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: 성공하면 목록을 만들어 `head`에 둔다. 끝에서 freeifaddrs로 돌려준다.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return None;
        }
        let mut total = (0, 0);
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: 목록의 항목은 freeifaddrs 전까지 유효하다.
            let ifa = unsafe { &*cur };
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_data.is_null() || ifa.ifa_name.is_null() {
                continue;
            }
            // SAFETY: 위에서 null이 아님을 보았다. AF_LINK 항목의 ifa_data는 if_data다.
            let (family, name) = unsafe {
                (
                    i32::from((*ifa.ifa_addr).sa_family),
                    std::ffi::CStr::from_ptr(ifa.ifa_name),
                )
            };
            if family != libc::AF_LINK || !name.to_bytes().starts_with(b"en") {
                continue;
            }
            // SAFETY: 같은 이유
            let data = unsafe { &*(ifa.ifa_data as *const libc::if_data) };
            total.0 += u64::from(data.ifi_ibytes);
            total.1 += u64::from(data.ifi_obytes);
        }
        // SAFETY: getifaddrs가 준 목록을 한 번 돌려준다.
        unsafe { libc::freeifaddrs(head) };
        Some(total)
    }

    fn process(&self, pid: u32) -> Option<ProcStat> {
        // SAFETY: proc_taskinfo는 정수만 든 C 구조체다.
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as libc::c_int;
        // SAFETY: 커널은 `size`바이트까지만 쓴다.
        let got = unsafe {
            libc::proc_pidinfo(
                libc::c_int::try_from(pid).ok()?,
                libc::PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if got != size {
            return None;
        }
        // Apple Silicon에서 이 값은 나노초가 아니라 Mach 시간 단위다.
        let ticks = u128::from(info.pti_total_user + info.pti_total_system);
        let (numer, denom) = self.timebase;
        Some(ProcStat {
            cpu_ns: (ticks * u128::from(numer) / u128::from(denom)) as u64,
            rss_mib: info.pti_resident_size >> 20,
        })
    }

    fn tap_bytes(&self, _name: &str) -> Option<(u64, u64)> {
        None
    }

    fn fs(&self, path: &Path) -> Option<(u64, u64)> {
        statvfs_mib(path)
    }
}

/// 그 밖의 OS: 아무것도 읽지 않는다.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug)]
pub struct NoProbe;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl HostProbe for NoProbe {
    fn cpu_times(&self) -> Option<CpuTimes> {
        None
    }
    fn memory(&self) -> Option<(u64, u64)> {
        None
    }
    fn disk_bytes(&self) -> Option<(u64, u64)> {
        None
    }
    fn nic_bytes(&self) -> Option<(u64, u64)> {
        None
    }
    fn process(&self, _pid: u32) -> Option<ProcStat> {
        None
    }
    fn tap_bytes(&self, _name: &str) -> Option<(u64, u64)> {
        None
    }
    fn fs(&self, _path: &Path) -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    const PROC_STAT: &str = "\
cpu  100 20 30 400 50 6 7 8 90 0
cpu0 50 10 15 200 25 3 3 4 45 0
intr 12345
";

    #[test]
    fn proc_stat_counts_busy_without_idle_and_iowait() {
        // guest(90)는 user에 이미 들어 있어 더하지 않는다.
        assert_eq!(
            parse_proc_stat(PROC_STAT),
            Some(CpuTimes {
                busy: 100 + 20 + 30 + 6 + 7 + 8,
                total: 621,
            })
        );
        assert_eq!(parse_proc_stat("intr 1\n"), None);
        assert_eq!(parse_proc_stat("cpu  1 x 3 4 5\n"), None);
    }

    #[test]
    fn meminfo_uses_available_memory() {
        let text = "MemTotal:       16384000 kB\nMemFree:          100000 kB\n\
                    MemAvailable:    8192000 kB\nBuffers:            1000 kB\n";
        assert_eq!(parse_meminfo(text), Some((8000, 16000)));
        assert_eq!(parse_meminfo("MemTotal: 1024 kB\n"), None);
    }

    #[test]
    fn diskstats_counts_only_real_disks() {
        let text = "\
   8       0 sda 100 0 1000 0 50 0 2000 0 0 0 0
   8       1 sda1 90 0 900 0 40 0 1800 0 0 0 0
 259       0 nvme0n1 10 0 10 0 5 0 20 0 0 0 0 0 0 0 0 0 0
 253       0 dm-0 70 0 700 0 30 0 1500 0 0 0 0
   7       0 loop0 5 0 50 0 0 0 0 0 0 0 0
   9       0 md0 1 0 100 0 1 0 100 0 0 0 0
 230       0 zd0 1 0 100 0 1 0 100 0 0 0 0
 252       0 rbd0 1 0 100 0 1 0 100 0 0 0 0
  43       0 nbd0 1 0 100 0 1 0 100 0 0 0 0
   1       0 ram0 1 0 100 0 1 0 100 0 0 0 0
 254       0 zram0 1 0 100 0 1 0 100 0 0 0 0
";
        // `/sys/block`에 있는 것: 디스크와 쌓인 장치(파티션 sda1은 없다)
        let in_sys_block = [
            "sda", "nvme0n1", "dm-0", "loop0", "md0", "zd0", "rbd0", "nbd0", "ram0", "zram0",
        ];
        let (read, written) = parse_diskstats(text, |n| in_sys_block.contains(&n));
        assert_eq!(read, (1000 + 10) * 512);
        assert_eq!(written, (2000 + 20) * 512);
        assert_eq!(parse_diskstats("garbage\n", |_| true), (0, 0));
    }

    #[test]
    fn pid_stat_skips_a_comm_with_spaces_and_parens() {
        let text = "4242 (qemu (vm) x) S 1 4242 4242 0 -1 4194560 100 0 0 0 \
                    700 300 0 0 20 0 5 0 1000 2000000 3000 18446744073709551615\n";
        assert_eq!(parse_pid_stat(text), Some(1000));
        assert_eq!(parse_pid_stat("4242 (qemu) S 1"), None);
    }

    #[test]
    fn status_rss_is_in_mib() {
        let text = "Name:\tqemu-system-x86\nVmPeak:\t 9000000 kB\nVmRSS:\t  2097152 kB\n";
        assert_eq!(parse_status_rss(text), Some(2048));
        assert_eq!(parse_status_rss("Name:\tkthreadd\n"), None);
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// 가짜 `/proc`·`/sys` 트리로 LinuxProbe가 파일을 맞게 고르는지 본다.
    #[test]
    fn linux_probe_reads_a_proc_and_sys_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "proc/stat", PROC_STAT);
        write(
            root,
            "proc/meminfo",
            "MemTotal: 2048 kB\nMemAvailable: 1024 kB\n",
        );
        write(
            root,
            "proc/diskstats",
            "8 0 sda 1 0 2 0 1 0 4 0 0 0 0\n8 1 sda1 1 0 2 0 1 0 4 0 0 0 0\n",
        );
        std::fs::create_dir_all(root.join("sys/block/sda")).unwrap();
        let stat = "7 (qemu) S 1 7 7 0 -1 0 0 0 0 0 250 50 0 0 20 0 1 0 1 1 1\n";
        write(root, "proc/7/stat", stat);
        write(root, "proc/7/status", "VmRSS:\t 3072 kB\n");
        // 오래 돈 큰 VM: tick × 10⁹이 u64를 넘는다.
        let stat = "9 (qemu) S 1 9 9 0 -1 0 0 0 0 0 60000000000 40000000000 0 0 20 0 1 0 1 1 1\n";
        write(root, "proc/9/stat", stat);
        write(root, "proc/9/status", "VmRSS:\t 1024 kB\n");
        // eth0은 실제 NIC, br0은 브리지(device 없음), opabcn0은 tap
        for (name, rx, tx, device) in [
            ("eth0", 1000, 2000, true),
            ("br0", 5, 5, false),
            ("opabcn0", 30, 40, false),
        ] {
            write(
                root,
                &format!("sys/class/net/{name}/statistics/rx_bytes"),
                &format!("{rx}\n"),
            );
            write(
                root,
                &format!("sys/class/net/{name}/statistics/tx_bytes"),
                &format!("{tx}\n"),
            );
            if device {
                std::fs::create_dir_all(root.join(format!("sys/class/net/{name}/device"))).unwrap();
            }
        }
        let probe = LinuxProbe::with_root(root);
        assert_eq!(probe.cpu_times().unwrap().total, 621);
        assert_eq!(probe.memory(), Some((1, 2)));
        assert_eq!(probe.disk_bytes(), Some((2 * 512, 4 * 512)));
        assert_eq!(probe.nic_bytes(), Some((1000, 2000)));
        assert_eq!(
            probe.process(7),
            Some(ProcStat {
                cpu_ns: 300 * 1_000_000_000 / probe.clock_ticks,
                rss_mib: 3,
            })
        );
        assert_eq!(
            probe.process(9).unwrap().cpu_ns,
            (100_000_000_000u128 * 1_000_000_000 / u128::from(probe.clock_ticks)) as u64
        );
        assert_eq!(probe.process(8), None);
        assert_eq!(probe.tap_bytes("opabcn0"), Some((30, 40)));
        assert_eq!(probe.tap_bytes("../../proc"), None);
        assert_eq!(probe.tap_bytes("nope"), None);
        let (used, total) = probe.fs(root).unwrap();
        assert!(total > 0 && used <= total);
    }

    /// 이 Mac에서 실제 값을 읽는다(값의 범위만 본다).
    #[cfg(target_os = "macos")]
    #[test]
    fn mac_probe_reads_this_host() {
        let probe = MacProbe::default();
        let cpu = probe.cpu_times().unwrap();
        assert!(cpu.total > 0 && cpu.busy <= cpu.total);
        let (used, total) = probe.memory().unwrap();
        assert_eq!(total, crate::host::memory_mib());
        assert!(used > 0 && used <= total);
        assert!(probe.nic_bytes().is_some());
        // 이 시험 프로세스: CPU를 조금 쓰고 RSS가 있다.
        let me = probe.process(std::process::id()).unwrap();
        assert!(me.cpu_ns > 0);
        assert!(me.rss_mib > 0 && me.rss_mib < total);
        assert_eq!(probe.process(u32::MAX), None);
        assert_eq!(probe.disk_bytes(), None);
        assert_eq!(probe.tap_bytes("en0"), None);
        let (used, total) = probe.fs(Path::new("/")).unwrap();
        assert!(total > 0 && used <= total);
    }

    /// Linux(CI)에서 실제 `/proc`·`/sys`를 읽는다(값의 범위만 본다).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_probe_reads_this_host() {
        let probe = LinuxProbe::default();
        let cpu = probe.cpu_times().unwrap();
        assert!(cpu.total > 0 && cpu.busy <= cpu.total);
        let (used, total) = probe.memory().unwrap();
        assert_eq!(total, crate::host::memory_mib());
        assert!(used <= total);
        assert!(probe.disk_bytes().is_some());
        assert!(probe.nic_bytes().is_some());
        let me = probe.process(std::process::id()).unwrap();
        assert!(me.rss_mib > 0 && me.rss_mib < total);
        let (used, total) = probe.fs(Path::new("/")).unwrap();
        assert!(total > 0 && used <= total);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn native_probe_reads_memory_here() {
        assert!(native().memory().is_some());
    }
}
