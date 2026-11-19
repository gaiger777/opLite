//! 스레드 CPU 친화(계획 64 §10): QEMU·swtpm 자식의 친화, vCPU 스레드 고정과 확인, opld 자신의 친화(`pin_tasks`).
//! `sched_setaffinity`는 Linux만이다. 다른 OS에서는 고정을 부르지 않는다(토폴로지가 없어 배치·고정이 생기지 않는다).

use std::path::Path;

use opl_types::numa::CpuSet;

/// `/proc/<pid>/task/<tid>/status`의 `Cpus_allowed_list`
pub fn allowed_list(status: &str) -> Option<CpuSet> {
    status
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .and_then(|v| v.trim().parse().ok())
}

/// systemd가 보이는 CPU 목록(`systemctl show --property=AllowedCPUs`): 범위를 빈칸으로 가른다(`0-1 4-15`). 비면 빈
/// 집합이다(걸지 않음).
pub fn systemd_cpus(text: &str) -> Option<CpuSet> {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(",")
        .parse()
        .ok()
}

/// 고정(`pinning`)이 없는 VM의 자식(QEMU·swtpm)에 걸 친화: opld 자신의 친화(`proc/self/status`)가 온라인 CPU
/// (`sys/devices/system/cpu/online`) 전체와 다르면(예약 CPU에 묶여 있음 — 노드 설정을 지운 직후 opld가 아직 풀리기 전
/// 등) 온라인 전체, 같거나 읽지 못하면 None(물려받아도 된다). 새 QEMU가 예약 CPU를 물려받지 않게 한다(리뷰 1).
pub fn unpinned_child(proc: &Path, sys: &Path) -> Option<CpuSet> {
    let own = allowed_list(&std::fs::read_to_string(proc.join("self/status")).ok()?)?;
    let online: CpuSet = std::fs::read_to_string(sys.join("devices/system/cpu/online"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (!online.is_empty() && own != online).then_some(online)
}

/// 스레드 `tid`(프로세스 `pid`)의 친화. 읽지 못하면(끝난 스레드) None
pub fn thread_cpus(proc: &Path, pid: u32, tid: u32) -> Option<CpuSet> {
    let path = proc.join(format!("{pid}/task/{tid}/status"));
    allowed_list(&std::fs::read_to_string(path).ok()?)
}

/// 커널이 만든 일꾼의 `stat` 플래그: `PF_USER_WORKER`(0x4000, vhost_task — `kvm-nx-lpage-recovery`·`vhost-<pid>`가
/// QEMU의 `task` 목록에 보인다), `PF_KTHREAD`(0x200000), `PF_NO_SETAFFINITY`(0x4000000)
const KERNEL_WORKER_FLAGS: u64 = 0x4000 | 0x20_0000 | 0x400_0000;

/// `/proc/<pid>/task/<tid>/stat` 한 줄이 커널 일꾼의 것인지(9번째 칸 `flags`). 이런 스레드의 친화는 커널이 정하고
/// (`sched_setaffinity`는 EINVAL이거나 커널이 되돌린다) 상한은 스코프의 cpuset(`AllowedCPUs`)이 쥔다.
pub fn is_kernel_worker_stat(stat: &str) -> bool {
    // `comm`(둘째 칸)에 빈칸·괄호가 들 수 있어 마지막 `)` 뒤부터 센다: state ppid pgrp session tty_nr tpgid flags
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return false;
    };
    rest.split_whitespace()
        .nth(6)
        .and_then(|f| f.parse::<u64>().ok())
        .is_some_and(|flags| flags & KERNEL_WORKER_FLAGS != 0)
}

/// 스레드 `tid`(프로세스 `pid`)가 커널 일꾼인지([`is_kernel_worker_stat`]). 읽지 못하면 false다.
pub fn is_kernel_worker(proc: &Path, pid: u32, tid: u32) -> bool {
    std::fs::read_to_string(proc.join(format!("{pid}/task/{tid}/stat")))
        .is_ok_and(|s| is_kernel_worker_stat(&s))
}

/// 프로세스 `pid`의 스레드 id
pub fn threads(proc: &Path, pid: u32) -> std::io::Result<Vec<u32>> {
    let mut out: Vec<u32> = std::fs::read_dir(proc.join(format!("{pid}/task")))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    out.sort_unstable();
    Ok(out)
}

/// QMP `query-cpus-fast`의 vCPU마다 (자리, 스레드 id). 자리는 `props`의 소켓·코어(`socket-id × per_socket + core-id`)이고
/// 없으면 `cpu-index`다. 스레드 id가 없는 vCPU는 뺀다.
pub fn vcpu_threads(cpus: &serde_json::Value, per_socket: u32) -> Vec<(usize, u32)> {
    let mut out: Vec<(usize, u32)> = cpus
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let tid = u32::try_from(c["thread-id"].as_u64()?).ok()?;
            let props = &c["props"];
            let slot = match (props["socket-id"].as_u64(), props["core-id"].as_u64()) {
                (Some(s), Some(core)) => s * u64::from(per_socket) + core,
                (Some(s), None) => s * u64::from(per_socket),
                _ => c["cpu-index"].as_u64()?,
            };
            Some((usize::try_from(slot).ok()?, tid))
        })
        .collect();
    out.sort_unstable();
    out
}

/// CPU 집합의 `cpu_set_t`. 상한(`CPU_SETSIZE`)을 넘는 번호는 뺀다.
#[cfg(target_os = "linux")]
pub fn mask(cpus: &CpuSet) -> libc::cpu_set_t {
    let max = usize::try_from(libc::CPU_SETSIZE).unwrap_or(1024);
    // SAFETY: cpu_set_t는 비트 배열이라 0으로 채운 값이 빈 집합이고, CPU_SET은 범위 안의 번호만 받는다.
    let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    for cpu in cpus.iter().map(usize::from).filter(|&c| c < max) {
        unsafe { libc::CPU_SET(cpu, &mut mask) };
    }
    mask
}

/// 스레드 `tid`(0이면 부르는 스레드)의 친화를 `mask`로 둔다. `fork` 뒤 `exec` 전(`pre_exec`)에도 부를 수 있다(시스템
/// 호출 하나).
#[cfg(target_os = "linux")]
pub fn set_mask(tid: libc::pid_t, mask: &libc::cpu_set_t) -> std::io::Result<()> {
    // SAFETY: mask는 cpu_set_t이고 크기를 함께 넘긴다.
    let rc = unsafe { libc::sched_setaffinity(tid, std::mem::size_of::<libc::cpu_set_t>(), mask) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 스레드 `tid`의 친화를 `cpus`로 둔다.
#[cfg(target_os = "linux")]
pub fn set_thread(tid: u32, cpus: &CpuSet) -> std::io::Result<()> {
    let tid = libc::pid_t::try_from(tid).map_err(std::io::Error::other)?;
    set_mask(tid, &mask(cpus))
}

#[cfg(not(target_os = "linux"))]
pub fn set_thread(_tid: u32, _cpus: &CpuSet) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "CPU pinning needs Linux",
    ))
}

/// 프로세스 `tasks`(`/proc/<pid>/task`)의 모든 스레드 친화를 `cpus`로 둔다. 바꾼 스레드 수를 돌려준다. 그사이 끝난
/// 스레드는 건너뛴다.
#[cfg(target_os = "linux")]
pub fn pin_tasks(tasks: &Path, cpus: &CpuSet) -> Result<usize, String> {
    let mask = mask(cpus);
    let entries =
        std::fs::read_dir(tasks).map_err(|e| format!("cannot read {}: {e}", tasks.display()))?;
    let mut pinned = 0;
    for tid in entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.parse::<libc::pid_t>().ok())
    {
        match set_mask(tid, &mask) {
            Ok(()) => pinned += 1,
            Err(e) if e.raw_os_error() == Some(libc::ESRCH) => {}
            Err(e) => return Err(format!("cannot set the CPU affinity of thread {tid}: {e}")),
        }
    }
    Ok(pinned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kernel_workers_are_told_by_the_stat_flags() {
        // 실제 x86 러너의 QEMU `task`: vCPU 스레드와 KVM의 vhost_task(PF_USER_WORKER 0x4000 등)
        let vcpu = "43301 (CPU 0/KVM) S 1 43298 43298 0 -1 4194368 2003 0 0 0 512 31 0 0 20 0 9 0";
        let nx = "43310 (kvm-nx-lpage-re) S 1 43298 43298 0 -1 4210752 0 0 0 0 0 0 0 0 20 0 9 0";
        let odd = "7 (a) b (c)) S 1 1 1 0 -1 2097216 0 0";
        assert!(!is_kernel_worker_stat(vcpu));
        assert!(is_kernel_worker_stat(nx));
        assert!(is_kernel_worker_stat(odd));
        assert!(!is_kernel_worker_stat("garbage"));
    }

    #[test]
    fn reads_the_allowed_list_of_a_thread() {
        let status =
            "Name:\tCPU 0/KVM\nCpus_allowed:\t4\nCpus_allowed_list:\t2\nMems_allowed:\t1\n";
        assert_eq!(allowed_list(status), Some("2".parse().unwrap()));
        assert_eq!(
            allowed_list("Cpus_allowed_list:\t0-3,8\n"),
            Some("0-3,8".parse().unwrap())
        );
        assert_eq!(allowed_list("Name:\tx\n"), None);
    }

    /// 리뷰 1 I4: systemd는 범위를 빈칸으로 가른다.
    #[test]
    fn systemd_cpu_lists_use_spaces() {
        assert_eq!(
            systemd_cpus("0-1 4-15\n"),
            Some("0-1,4-15".parse().unwrap())
        );
        assert_eq!(systemd_cpus("3"), Some("3".parse().unwrap()));
        assert_eq!(systemd_cpus(""), Some(CpuSet::new()));
        assert_eq!(systemd_cpus("x y"), None);
    }

    /// 리뷰 1 I5: opld가 온라인 CPU 전체보다 좁게 묶여 있으면 고정 없는 자식은 온라인 전체로 둔다.
    #[test]
    fn unpinned_children_get_every_online_cpu_when_opld_is_narrowed() {
        let tmp = tempfile::tempdir().unwrap();
        let (proc, sys) = (tmp.path().join("proc"), tmp.path().join("sys"));
        std::fs::create_dir_all(proc.join("self")).unwrap();
        std::fs::create_dir_all(sys.join("devices/system/cpu")).unwrap();
        std::fs::write(sys.join("devices/system/cpu/online"), "0-7\n").unwrap();
        let status = |cpus: &str| {
            std::fs::write(
                proc.join("self/status"),
                format!("Name:\topld\nCpus_allowed_list:\t{cpus}\n"),
            )
            .unwrap()
        };
        status("0-1");
        assert_eq!(unpinned_child(&proc, &sys), Some("0-7".parse().unwrap()));
        status("0-7");
        assert_eq!(unpinned_child(&proc, &sys), None);
        assert_eq!(unpinned_child(&tmp.path().join("none"), &sys), None);
    }

    #[test]
    fn vcpu_slots_come_from_the_socket_and_core() {
        let cpus = json!([
            { "cpu-index": 0, "thread-id": 101, "props": { "socket-id": 0, "core-id": 0, "thread-id": 0 } },
            { "cpu-index": 1, "thread-id": 102, "props": { "socket-id": 1, "core-id": 1, "thread-id": 0 } },
            // 실행 중 붙인 vCPU: cpu-index는 자리와 다를 수 있다.
            { "cpu-index": 2, "thread-id": 103, "props": { "socket-id": 0, "core-id": 3, "thread-id": 0 } },
            { "cpu-index": 3, "props": { "socket-id": 1, "core-id": 0 } },
        ]);
        assert_eq!(vcpu_threads(&cpus, 4), [(0, 101), (3, 103), (5, 102)]);
        let plain = json!([{ "cpu-index": 1, "thread-id": 7, "props": {} }]);
        assert_eq!(vcpu_threads(&plain, 4), [(1, 7)]);
    }
}
