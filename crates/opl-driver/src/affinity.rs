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

/// 스레드 `tid`(프로세스 `pid`)의 친화. 읽지 못하면(끝난 스레드) None
pub fn thread_cpus(proc: &Path, pid: u32, tid: u32) -> Option<CpuSet> {
    let path = proc.join(format!("{pid}/task/{tid}/status"));
    allowed_list(&std::fs::read_to_string(path).ok()?)
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
