//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::Path;
use std::time::Duration;

use opl_driver::{
    ActualState, EventKind, Hypervisor, HypervisorEvent, QemuConfig, QemuHypervisor, RuntimeStatus,
    StopMode,
};
use opl_qemu::HostProfile;
use opl_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use tokio::sync::broadcast;
use uuid::Uuid;

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // systemd가 있는 Linux(CI)에서 `OPL_TEST_SYSTEMD=1`로 스코프 실행까지 확인한다.
        systemd_scope: std::env::var_os("OPL_TEST_SYSTEMD").is_some(),
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
        machine: Arch::native().machine(),
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

    hv.start(&vm, None).await.unwrap();
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    hv.start(&vm, None).await.unwrap(); // 이미 실행 중이면 그대로 성공

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
        hv.start(&vm, None).await.unwrap();
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
    hv.start(&vm, None).await.unwrap();
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
        a.start(vm, None).await.unwrap();
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
    let err = b.start(&vm, None).await.unwrap_err();
    assert!(err.is_transient(), "{err:?}");
    assert!(run_dir.join("qemu.pid").exists());
    assert!(run_dir.join("qmp.sock").exists());
    assert!(process_exists(pid));

    assert!(signal("-CONT", pid));
    b.start(&vm, None).await.unwrap();
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
    hv.start(&vm, None).await.unwrap();
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

/// `path`에 OS 없는 1GiB qcow2 디스크를 만들어 VM 스펙에 붙인다.
fn with_qcow2_disk(mut vm: VmRuntimeSpec, path: &Path) -> VmRuntimeSpec {
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(path)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
    vm.disks.push(RuntimeDisk {
        path: path.to_path_buf(),
        kind: DiskKind::File,
        rbd: None,
        format: ImageFormat::Qcow2,
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    });
    vm
}

/// 디스크 이미지의 내부 스냅샷 이름과 가상 크기 (QEMU가 쓰는 중이어도 읽는다)
fn image_info(path: &Path) -> (Vec<String>, u64) {
    let out = std::process::Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success());
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let tags = info["snapshots"]
        .as_array()
        .map(|s| {
            s.iter()
                .map(|s| s["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    (tags, info["virtual-size"].as_u64().unwrap())
}

#[tokio::test]
async fn snapshots_of_running_vm_and_start_restoring_memory() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    hv.start(&vm, None).await.unwrap();

    // 실행 중 크기 늘리기
    hv.block_resize(vm.id, 0, 2 << 30).await.unwrap();
    assert_eq!(image_info(&disk).1, 2 << 30);

    // 메모리 스냅샷: 저장하는 동안 멈췄다가 다시 실행된다.
    hv.snapshot_save(vm.id, "mem", true).await.unwrap();
    let st = hv.inspect(vm.id).await.unwrap().unwrap();
    assert_eq!(st.status, RuntimeStatus::Running);
    // 디스크만 스냅샷
    hv.snapshot_save(vm.id, "disk", false).await.unwrap();
    assert_eq!(image_info(&disk).0, ["mem", "disk"]);
    hv.snapshot_delete(vm.id, "disk").await.unwrap();
    // 없는 태그를 지워도 성공한다.
    hv.snapshot_delete(vm.id, "disk").await.unwrap();
    assert_eq!(image_info(&disk).0, ["mem"]);

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);
    let mut rx = hv.subscribe();

    // 없는 태그를 불러오면 QEMU를 끝내고 다시 시도하지 않을 오류로 끝난다.
    let err = hv.start(&vm, Some("nope")).await.unwrap_err();
    assert!(
        matches!(
            err,
            opl_driver::DriverError::Qmp(opl_qemu::QmpError::JobFailed { .. })
        ),
        "{err:?}"
    );
    assert!(!err.is_transient());
    assert_eq!(next_event_for(&mut rx, vm.id).await, EventKind::Started);
    assert!(matches!(
        next_event_for(&mut rx, vm.id).await,
        EventKind::Stopped { .. } | EventKind::Crashed
    ));
    assert_eq!(hv.inspect(vm.id).await.unwrap(), None);

    // 메모리 스냅샷을 불러오며 시작하면 실행 상태다.
    hv.start(&vm, Some("mem")).await.unwrap();
    let st = hv.inspect(vm.id).await.unwrap().unwrap();
    assert_eq!(st.status, RuntimeStatus::Running);
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

#[tokio::test]
async fn memory_snapshot_needs_a_disk() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let err = hv.snapshot_save(vm.id, "mem", true).await.unwrap_err();
    assert!(
        matches!(err, opl_driver::DriverError::InvalidSpec(_)),
        "{err:?}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돈다.
#[tokio::test]
async fn qemu_runs_in_its_own_systemd_scope() {
    if std::env::var_os("OPL_TEST_SYSTEMD").is_none() {
        return;
    }
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let vm = spec();
    hv.start(&vm, None).await.unwrap();
    let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(
        cgroup.contains(&format!("opl-vm-{}.scope", vm.id)),
        "{cgroup}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    // 끝난 스코프가 아직 남아 있어도 시작 전에 지우므로 곧바로 다시 시작할 수 있다.
    hv.start(&vm, None).await.unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}
