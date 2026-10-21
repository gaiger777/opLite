//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! Apple Silicon Mac + `brew install qemu`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::Path;
use std::time::Duration;

use opl_driver::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, QemuConfig, QemuHypervisor, RuntimeStatus,
    StopMode,
};
use opl_qemu::HostProfile;
use opl_types::runtime::VmRuntimeSpec;
use opl_types::vm::{CpuModel, CpuSpec, Firmware, MachineType};
use tokio::sync::broadcast;
use uuid::Uuid;

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::homebrew_hvf(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
    }
}

fn spec() -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "it-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: MachineType::Virt,
        disks: vec![],
        nics: vec![],
    }
}

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

async fn next_event_for(rx: &mut broadcast::Receiver<HypervisorEvent>, id: Uuid) -> EventKind {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let ev = rx.recv().await.unwrap();
            if ev.id == id {
                return ev.kind;
            }
        }
    })
    .await
    .expect("no event within 10s")
}

#[tokio::test]
async fn start_inspect_and_force_stop() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let mut rx = hv.subscribe();
    let vm = spec();

    hv.start(&vm).await.unwrap();
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    hv.start(&vm).await.unwrap(); // 이미 실행 중이면 그대로 성공

    let st = hv.inspect(vm.id).await.unwrap().expect("running");
    assert_eq!(st.status, RuntimeStatus::Running);
    assert!(st.pid > 0);
    assert_eq!(hv.list().await.unwrap().len(), 1);
    assert!(hv.console_endpoint(vm.id).await.unwrap().exists());

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(
        next_event_for(&mut rx, vm.id).await,
        EventKind::Stopped {
            guest_initiated: false
        }
    );
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
    assert!(
        tmp.path()
            .join("state/vm")
            .join(vm.id.to_string())
            .join("uefi-vars.fd")
            .exists()
    );
}

#[tokio::test]
async fn reattaches_after_driver_restart() {
    let tmp = short_tempdir();
    let vm = spec();
    let pid = {
        let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
        hv.start(&vm).await.unwrap();
        hv.inspect(vm.id).await.unwrap().unwrap().pid
        // 드라이버(= opld)가 내려간다. QEMU는 분리되어 있으므로 계속 돌아야 한다.
    };

    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let st = hv
        .inspect(vm.id)
        .await
        .unwrap()
        .expect("VM should survive driver restart");
    assert_eq!(st.pid, pid);
    assert_eq!(st.status, RuntimeStatus::Running);

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
}

#[tokio::test]
async fn reports_crash_when_qemu_is_killed() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let mut rx = hv.subscribe();

    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Crashed);
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
}

fn signal(sig: &str, pid: u32) -> bool {
    std::process::Command::new("kill")
        .args([sig, &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn process_exists(pid: u32) -> bool {
    signal("-0", pid)
}

/// 테스트가 중간에 실패해도 멈춰 둔 QEMU가 남지 않게 drop에서 SIGKILL을 보낸다.
/// 이미 끝났으면 pid가 재사용됐을 수 있으므로 아무것도 하지 않는다.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if process_exists(self.0) {
            signal("-KILL", self.0);
        }
    }
}

/// 드라이버 A로 VM을 띄우고 A를 내린 뒤 QEMU를 SIGSTOP한다. 재시작한 opld가 멈춘 VM을 만나는 상황이다.
async fn start_then_stop_process(root: &Path, vm: &VmRuntimeSpec) -> (u32, KillOnDrop) {
    let pid = {
        let a = QemuHypervisor::new(config(root)).await.unwrap();
        a.start(vm).await.unwrap();
        a.inspect(vm.id).await.unwrap().unwrap().pid
    };
    let guard = KillOnDrop(pid);
    assert!(signal("-STOP", pid));
    (pid, guard)
}

fn unresponsive(id: Uuid, pid: u32) -> ActualState {
    ActualState {
        id,
        pid,
        status: RuntimeStatus::Other("unresponsive".into()),
    }
}

#[tokio::test]
async fn start_does_not_destroy_unresponsive_running_vm() {
    let tmp = short_tempdir();
    let vm = spec();
    let run_dir = tmp.path().join("run/vm").join(vm.id.to_string());
    let (pid, _guard) = start_then_stop_process(tmp.path(), &vm).await;

    // 멈춘 QEMU는 QMP에 답하지 않는다. 살아 있으므로 파일을 지우지 않고 등록하지 않지만,
    // 조회에는 pidfile 잠금 소유자(= QEMU) pid와 함께 드러난다.
    let b = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    assert!(run_dir.exists());
    assert_eq!(
        b.inspect(vm.id).await.unwrap(),
        Some(unresponsive(vm.id, pid))
    );

    // 새로 띄우거나 파일을 지우지 않고, 다시 시도할 수 있는 오류로 끝난다.
    let err = b.start(&vm).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert!(run_dir.join("qemu.pid").exists());
    assert!(run_dir.join("qmp.sock").exists());
    assert!(process_exists(pid));

    assert!(signal("-CONT", pid));
    b.start(&vm).await.unwrap();
    let st = b.inspect(vm.id).await.unwrap().expect("reattached");
    assert_eq!(st.pid, pid);
    assert_eq!(st.status, RuntimeStatus::Running);

    b.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(b.inspect(vm.id).await.unwrap(), None);
    assert!(!run_dir.exists());
}

#[tokio::test]
async fn force_stop_kills_unresponsive_qemu() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let _guard = KillOnDrop(pid);
    let mut rx = hv.subscribe();
    assert!(signal("-STOP", pid));
    // QMP에 답하지 않아도 살아 있는 VM으로 드러난다.
    assert_eq!(
        hv.inspect(vm.id).await.unwrap(),
        Some(unresponsive(vm.id, pid))
    );

    tokio::time::timeout(Duration::from_secs(30), hv.stop(vm.id, StopMode::Force))
        .await
        .expect("force stop took longer than 30s")
        .unwrap();
    // SHUTDOWN 이벤트 없이 죽였으므로 Crashed로 보고된다.
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Crashed);
    // launchd/init이 회수할 때까지 잠시 걸릴 수 있다.
    let mut gone = false;
    for _ in 0..100 {
        if !process_exists(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "QEMU {pid} is still alive");
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    assert!(!tmp.path().join("run/vm").join(vm.id.to_string()).exists());
}

#[tokio::test]
async fn force_stop_after_restart_kills_stopped_qemu() {
    let tmp = short_tempdir();
    let vm = spec();
    let run_dir = tmp.path().join("run/vm").join(vm.id.to_string());
    let (pid, _guard) = start_then_stop_process(tmp.path(), &vm).await;

    let b = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    assert_eq!(b.list().await.unwrap(), vec![unresponsive(vm.id, pid)]);
    // QMP 없이는 ACPI 요청을 보낼 수 없다.
    assert!(
        b.stop(vm.id, StopMode::Acpi)
            .await
            .unwrap_err()
            .is_transient()
    );

    tokio::time::timeout(Duration::from_secs(30), b.stop(vm.id, StopMode::Force))
        .await
        .expect("force stop took longer than 30s")
        .unwrap();
    let mut gone = false;
    for _ in 0..100 {
        if !process_exists(pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "QEMU {pid} is still alive");
    assert!(!run_dir.exists());
    assert_eq!(b.inspect(vm.id).await.unwrap(), None);
    assert!(b.list().await.unwrap().is_empty());
}
