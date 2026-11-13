//! 실제 QEMU를 띄우는 드라이버 통합 테스트. `cargo test -p opl-driver --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::Path;
use std::time::Duration;

use opl_driver::{
    ActualState, BackupTarget, DirStorage, EventKind, Hypervisor, HypervisorEvent, QemuConfig,
    QemuHypervisor, RuntimeStatus, StopMode, Storage, VolumeSource,
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

/// 다른 QEMU(다른 노드)가 쓰는 이미지로 켜면 잠금 오류로 구분된다(실제 QEMU 문구 확인).
#[tokio::test]
async fn image_locked_by_another_qemu_is_transient() {
    let tmp = short_tempdir();
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    let a = QemuHypervisor::new(config(&tmp.path().join("a")))
        .await
        .unwrap();
    a.start(&vm, None).await.unwrap();

    // 같은 디스크를 쓰는 다른 VM(systemd 스코프 이름이 겹치지 않게 다른 id)
    let other = VmRuntimeSpec {
        id: Uuid::now_v7(),
        ..vm.clone()
    };
    let b = QemuHypervisor::new(config(&tmp.path().join("b")))
        .await
        .unwrap();
    let err = b.start(&other, None).await.unwrap_err();
    assert!(
        matches!(err, opl_driver::DriverError::ImageLocked(_)),
        "{err:?}"
    );
    assert!(err.is_transient());
    assert_eq!(b.inspect(other.id).await.unwrap(), None);

    // 잠금이 풀리면 켤 수 있다.
    a.stop(vm.id, StopMode::Force).await.unwrap();
    b.start(&other, None).await.unwrap();
    b.stop(other.id, StopMode::Force).await.unwrap();
}

/// `OPL_TEST_SYSTEMD=1`(systemd가 있는 Linux)일 때만 확인한다: QEMU가 VM 전용 스코프에서 돌고, 그 스코프는
/// `opld.service` 앞에 묶였다.
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
    // 호스트가 꺼질 때 opld가 먼저 끝나도록 스코프가 opld.service 앞에 묶였다.
    let show = std::process::Command::new("systemctl")
        .args(["show", "-p", "Before", &format!("opl-vm-{}.scope", vm.id)])
        .output()
        .unwrap();
    let before = String::from_utf8_lossy(&show.stdout);
    assert!(
        before
            .split_whitespace()
            .any(|u| u.ends_with("opld.service")),
        "{before}"
    );
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    // 끝난 스코프가 아직 남아 있어도 시작 전에 지우므로 곧바로 다시 시작할 수 있다.
    hv.start(&vm, None).await.unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

/// 이미지에 `qemu-io` 명령 하나(꺼진 VM의 디스크에 쓰기, 읽어 확인하기). `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, cmd: &str) -> bool {
    std::process::Command::new("qemu-io")
        .args(["-f", "qcow2", "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 디스크 노드마다 비트맵 이름
async fn bitmap_names(hv: &QemuHypervisor, id: Uuid) -> Vec<Vec<String>> {
    hv.backup_bitmaps(id)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.bitmaps.into_iter().map(|b| b.name).collect())
        .collect()
}

/// 증분 백업: 전체 백업의 시점에 만든 비트맵(qcow2라 VM을 껐다 켜도 남는다) 뒤에 바뀐 블록만 증분
/// 파일에 담기고, 전체·증분 체인을 가져오면 디스크와 같다. 실패하면 앞 비트맵이 남아 다시 할 수 있다.
#[tokio::test]
async fn incremental_backup_holds_only_changed_blocks() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let storage = DirStorage::new("qemu-img", tmp.path().join("pools"));
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    assert!(qemu_io(&disk, "write -P 0xab 0 1M"));
    hv.start(&vm, None).await.unwrap();

    // 전체 백업: 스냅샷과 비트맵을 한 시점에 만들고 스냅샷을 내보낸다.
    hv.snapshot_save_with_bitmap(vm.id, "full", "opl-a")
        .await
        .unwrap();
    let disks = hv.backup_bitmaps(vm.id).await.unwrap();
    assert_eq!(disks.len(), 1);
    assert_eq!(disks[0].node, "disk0");
    let a = &disks[0].bitmaps[0];
    assert_eq!(
        (a.name.as_str(), a.recording, a.inconsistent, a.persistent),
        ("opl-a", true, false, true)
    );
    let full = tmp.path().join("full.qcow2");
    let source = VolumeSource {
        path: disk.clone(),
        format: ImageFormat::Qcow2,
        snapshot: Some("full".into()),
        size_bytes: 1 << 30,
        rbd: None,
    };
    storage.export_volume(&source, true, &full).await.unwrap();
    hv.snapshot_delete(vm.id, "full").await.unwrap();

    // 게스트가 없으므로 VM을 끄고 쓴다. 저장된 비트맵을 연 qemu-io의 쓰기도 비트맵에 남는다.
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    assert!(qemu_io(&disk, "write -P 0xcd 512k 64k"));
    assert!(qemu_io(&disk, "write -P 0xef 600M 64k"));
    hv.start(&vm, None).await.unwrap();
    let a = &hv.backup_bitmaps(vm.id).await.unwrap()[0].bitmaps[0];
    assert_eq!(
        (a.name.as_str(), a.recording, a.inconsistent),
        ("opl-a", true, false)
    );

    // 없는 비트맵에서는 실패하고, 대상 노드를 닫고 비트맵은 그대로다.
    let incr = tmp.path().join("incr.qcow2");
    storage.create_backup_target(&incr, 1 << 30).await.unwrap();
    let targets = [BackupTarget {
        node: "disk0".into(),
        path: incr.clone(),
        size: 1 << 30,
    }];
    let err = hv
        .incremental_backup(vm.id, "opl-x", "opl-b", &targets)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("opl-x"), "{err}");
    assert_eq!(bitmap_names(&hv, vm.id).await, [["opl-a"]]);

    hv.incremental_backup(vm.id, "opl-a", "opl-b", &targets)
        .await
        .unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [["opl-b"]]);
    hv.stop(vm.id, StopMode::Force).await.unwrap();

    // 증분 파일에는 바뀐 블록만 있다(나머지는 비어 0으로 읽힌다).
    assert!(qemu_io(&incr, "read -P 0xcd 512k 64k"));
    assert!(qemu_io(&incr, "read -P 0xef 600M 64k"));
    assert!(qemu_io(&incr, "read -P 0 0 512k"));
    assert!(std::fs::metadata(&incr).unwrap().len() < 1 << 20);

    let pools = tmp.path().join("restore");
    std::fs::create_dir(&pools).unwrap();
    let pool = opl_types::resource::Resource::new(
        "restore",
        opl_types::storage::StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir { path: pools },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    let restored = storage
        .import_chain(&pool, Uuid::now_v7(), &[incr, full], ImageFormat::Qcow2)
        .await
        .unwrap();
    let same = std::process::Command::new("qemu-img")
        .args(["compare", "-q", "-f", "qcow2", "-F", "qcow2"])
        .args([&restored, &disk])
        .status()
        .unwrap();
    assert!(same.success(), "restored chain differs from the disk");

    // 비트맵 더하기·지우기
    hv.start(&vm, None).await.unwrap();
    hv.add_bitmaps(vm.id, "opl-c").await.unwrap();
    hv.remove_bitmaps(vm.id, Some("opl-c")).await.unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [["opl-c"]]);
    hv.remove_bitmaps(vm.id, None).await.unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [Vec::<String>::new()]);
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

/// raw 디스크의 비트맵은 QEMU 안에만 있어 VM을 다시 켜면 사라진다. 바뀐 것이 없으면 증분도 비어 있다.
#[tokio::test]
async fn raw_disk_bitmaps_do_not_survive_a_restart() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let storage = DirStorage::new("qemu-img", tmp.path().join("pools"));
    let disk = tmp.path().join("disk0.raw");
    std::fs::File::create(&disk)
        .unwrap()
        .set_len(64 << 20)
        .unwrap();
    let mut vm = spec();
    vm.disks.push(RuntimeDisk {
        path: disk,
        kind: DiskKind::File,
        rbd: None,
        format: ImageFormat::Raw,
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    });
    hv.start(&vm, None).await.unwrap();
    hv.add_bitmaps(vm.id, "opl-a").await.unwrap();
    let a = &hv.backup_bitmaps(vm.id).await.unwrap()[0].bitmaps[0];
    assert_eq!((a.name.as_str(), a.persistent), ("opl-a", false));

    let incr = tmp.path().join("incr.qcow2");
    storage.create_backup_target(&incr, 64 << 20).await.unwrap();
    let targets = [BackupTarget {
        node: "disk0".into(),
        path: incr.clone(),
        size: 64 << 20,
    }];
    hv.incremental_backup(vm.id, "opl-a", "opl-b", &targets)
        .await
        .unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [["opl-b"]]);
    assert!(qemu_io(&incr, "read -P 0 0 64M"));

    hv.stop(vm.id, StopMode::Force).await.unwrap();
    hv.start(&vm, None).await.unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [Vec::<String>::new()]);
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}

/// QEMU에 SIGTERM을 보내 깨끗이 끝내고(QMP `quit`처럼 비트맵을 이미지에 쓴다) 프로세스가 사라지기를
/// 기다린다. `StopMode::Force`와 달리 오래 걸려도 SIGKILL을 보내지 않는다.
async fn stop_cleanly(hv: &QemuHypervisor, id: Uuid) {
    let pid = hv.inspect(id).await.unwrap().unwrap().pid;
    let mut rx = hv.subscribe();
    assert!(signal("-TERM", pid));
    let stopped = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let ev = rx.recv().await.unwrap();
            if ev.id == id {
                return ev.kind;
            }
        }
    })
    .await
    .expect("QEMU did not stop within 120s");
    assert_eq!(
        stopped,
        EventKind::Stopped {
            guest_initiated: false
        }
    );
    tokio::time::timeout(Duration::from_secs(120), async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("QEMU did not exit within 120s");
}

/// 증분이 0 쓰기(`write -z`, 할당을 푼 것 포함)와 discard도 담는다: 전체 위에 증분을 덮은 체인이 디스크와
/// 같다.
#[tokio::test]
async fn incremental_backup_holds_zeroed_and_discarded_blocks() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let storage = DirStorage::new("qemu-img", tmp.path().join("pools"));
    let disk = tmp.path().join("disk0.qcow2");
    let vm = with_qcow2_disk(spec(), &disk);
    assert!(qemu_io(&disk, "write -P 0xab 0 4M"));
    hv.start(&vm, None).await.unwrap();
    hv.snapshot_save_with_bitmap(vm.id, "full", "opl-a")
        .await
        .unwrap();
    let full = tmp.path().join("full.qcow2");
    let source = VolumeSource {
        path: disk.clone(),
        format: ImageFormat::Qcow2,
        snapshot: Some("full".into()),
        size_bytes: 1 << 30,
        rbd: None,
    };
    storage.export_volume(&source, true, &full).await.unwrap();
    hv.snapshot_delete(vm.id, "full").await.unwrap();

    // 실행 중에 더한 persistent 비트맵은 QEMU가 끝날 때 이미지에 쓴다. Force는 부하가 크면 SIGKILL로
    // 떨어져 비트맵을 잃으므로 깨끗이 끝낸다.
    stop_cleanly(&hv, vm.id).await;
    for cmd in [
        "write -z 0 64k",
        "write -z -u 128k 64k",
        "discard 256k 64k",
        "write -P 0xcd 512k 64k",
    ] {
        assert!(qemu_io(&disk, cmd), "{cmd}");
    }
    hv.start(&vm, None).await.unwrap();
    assert!(
        bitmap_names(&hv, vm.id).await[0].contains(&"opl-a".to_string()),
        "bitmap lost — VM was not stopped cleanly"
    );
    let incr = tmp.path().join("incr.qcow2");
    storage.create_backup_target(&incr, 1 << 30).await.unwrap();
    let targets = [BackupTarget {
        node: "disk0".into(),
        path: incr.clone(),
        size: 1 << 30,
    }];
    hv.incremental_backup(vm.id, "opl-a", "opl-b", &targets)
        .await
        .unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();

    let pools = tmp.path().join("restore");
    std::fs::create_dir(&pools).unwrap();
    let pool = opl_types::resource::Resource::new(
        "restore",
        opl_types::storage::StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir { path: pools },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    let restored = storage
        .import_chain(&pool, Uuid::now_v7(), &[incr, full], ImageFormat::Qcow2)
        .await
        .unwrap();
    let same = std::process::Command::new("qemu-img")
        .args(["compare", "-q", "-f", "qcow2", "-F", "qcow2"])
        .args([&restored, &disk])
        .status()
        .unwrap();
    assert!(same.success(), "restored chain differs from the disk");
    assert!(qemu_io(&restored, "read -P 0 0 64k"));
    assert!(qemu_io(&restored, "read -P 0 256k 64k"));
}

/// QEMU가 비정상으로 끝나 이미지에 inconsistent로 남은 비트맵이 있어도 꺼진 디스크의 크기를 늘린다
/// (증분 백업의 비트맵을 지운다). 다시 켠 VM에는 그런 비트맵이 남지 않는다.
#[tokio::test]
async fn inconsistent_bitmaps_do_not_block_offline_resize_or_restart() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let pools = tmp.path().join("pool");
    std::fs::create_dir(&pools).unwrap();
    let pool = opl_types::resource::Resource::new(
        "pool",
        opl_types::storage::StoragePoolSpec {
            backend: opl_types::storage::PoolBackend::Dir {
                path: pools.clone(),
            },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    let storage = DirStorage::new("qemu-img", tmp.path().join("mnt"));
    let id = Uuid::now_v7();
    let disk = pools.join(format!("{id}.qcow2"));
    let vm = with_qcow2_disk(spec(), &disk);
    let mut rx = hv.subscribe();
    // 깨끗이 끝나면 비트맵을 이미지에 저장하고, 다시 연 QEMU는 그것을 in-use로 표시한 채 쓴다.
    let crash_with_bitmap = async |name: &str, rx: &mut broadcast::Receiver<HypervisorEvent>| {
        hv.start(&vm, None).await.unwrap();
        hv.add_bitmaps(vm.id, name).await.unwrap();
        hv.stop(vm.id, StopMode::Force).await.unwrap();
        hv.start(&vm, None).await.unwrap();
        let pid = hv.inspect(vm.id).await.unwrap().unwrap().pid;
        assert!(signal("-KILL", pid));
        while next_event_for(rx, vm.id).await != EventKind::Crashed {}
        // Linux에서 pidfile 잠금(POSIX)은 fd를 닫을 때 바로 풀리지만 이미지 잠금(OFD)은 파일의 마지막
        // 참조가 사라질 때 풀려 Crashed 뒤에도 잠깐 남을 수 있다. 프로세스가 사라질 때까지 기다린다.
        tokio::time::timeout(Duration::from_secs(10), async {
            while process_exists(pid) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("killed QEMU did not exit within 10s");
    };
    crash_with_bitmap("opl-a", &mut rx).await;
    // 이미지에 in-use로 남아 qemu-img도 크기를 바꾸지 못한다.
    let out = std::process::Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(&disk)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "qemu-img info: {}: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let flags = &info["format-specific"]["data"]["bitmaps"][0]["flags"];
    assert!(
        flags
            .as_array()
            .is_some_and(|f| f.iter().any(|f| f == "in-use")),
        "{info}"
    );

    storage
        .resize_volume(&pool, id, ImageFormat::Qcow2, 2 << 30)
        .await
        .unwrap();
    assert_eq!(image_info(&disk).1, 2 << 30);

    // 다시 inconsistent 비트맵을 남기고 켠다: 지워진다.
    crash_with_bitmap("opl-b", &mut rx).await;
    hv.start(&vm, None).await.unwrap();
    assert_eq!(bitmap_names(&hv, vm.id).await, [Vec::<String>::new()]);
    hv.block_resize(vm.id, 0, 3 << 30).await.unwrap();
    hv.stop(vm.id, StopMode::Force).await.unwrap();
}
