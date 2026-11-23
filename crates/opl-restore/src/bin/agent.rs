//! `orbivisor-restore-agent`: 파일 복원 도우미 VM의 PID 1(설계 §9.1). 도우미 이미지의 initramfs에 정적 musl 바이너리로
//! 들어간다(`cargo build --release -p opl-restore --bin orbivisor-restore-agent --target <arch>-unknown-linux-musl`).
//! Linux가 아닌 곳에서는 할 일이 없다.

#[cfg(target_os = "linux")]
fn main() {
    opl_restore::agent::linux::run_pid1()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!(
        "orbivisor-restore-agent runs only as PID 1 of the file restore helper VM (Linux); \
         build it with --target <arch>-unknown-linux-musl"
    );
    std::process::exit(2);
}
