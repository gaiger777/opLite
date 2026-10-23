//! 실제 QEMU를 띄우는 통합 테스트. `cargo test -p opl-qemu --features qemu-it`
//! macOS(Apple Silicon)는 `brew install qemu`, Linux는 `apt install qemu-system ovmf qemu-efi-aarch64 qemu-utils`가 필요하다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_qemu::{Arch, HostProfile, QmpClient, RunState, RuntimePaths, build_cmdline};
use opl_types::runtime::{ImageFormat, NicBackend, RuntimeDisk, RuntimeNic, VmRuntimeSpec};
use opl_types::vm::{CpuModel, CpuSpec, DiskBus, Firmware, NicModel};
use uuid::Uuid;

fn host() -> HostProfile {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
    HostProfile::native(Path::new(&share))
}

fn spec(id: Uuid, disk: PathBuf) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id,
        name: "it-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![RuntimeDisk {
            path: disk,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![RuntimeNic {
            mac: "52:54:00:12:34:56".parse().unwrap(),
            model: NicModel::Virtio,
            backend: NicBackend::User,
        }],
    }
}

async fn wait_for(path: &Path) {
    for _ in 0..100 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{path:?} did not appear within 10s");
}

#[tokio::test]
async fn boots_paused_resumes_reconnects_and_quits() {
    let host = host();
    // macOS의 기본 TMPDIR은 경로가 길어 소켓 경로 한도(103바이트)를 넘기므로 /tmp를 쓴다.
    let tmp = tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap();
    let id = Uuid::now_v7();
    let paths = RuntimePaths::new(tmp.path(), id).unwrap();
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::copy(&host.uefi_vars_template, &paths.uefi_vars).unwrap();

    let disk = tmp.path().join("root.qcow2");
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());

    let args = build_cmdline(&spec(id, disk), &host, &paths).unwrap();
    let mut child = tokio::process::Command::new(&host.qemu_binary)
        .args(&args)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for(&paths.qmp_socket).await;

    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Prelaunch);
    qmp.cont().await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);

    // opld 재시작을 흉내 낸다: 연결을 끊고 다시 붙어도 VM은 계속 실행 중이어야 한다.
    drop(qmp);
    let qmp = QmpClient::connect(&paths.qmp_socket).await.unwrap();
    assert_eq!(qmp.query_status().await.unwrap(), RunState::Running);

    qmp.quit().await.unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .expect("qemu did not exit within 10s")
        .unwrap();
    assert!(exit.success(), "qemu exited with {exit}");
}
