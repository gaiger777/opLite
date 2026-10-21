//! 호스트 용량. 스케줄러가 노드를 고를 때 쓴다.

/// 물리 메모리(MiB). 알 수 없으면 0이다.
pub fn memory_mib() -> u64 {
    bytes().map_or(0, |b| b >> 20)
}

#[cfg(target_os = "linux")]
fn bytes() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = info.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(target_os = "macos")]
fn bytes() -> Option<u64> {
    let out = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_host_memory() {
        assert!(super::memory_mib() > 0);
    }
}
