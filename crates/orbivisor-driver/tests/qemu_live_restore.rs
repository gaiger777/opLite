//! 실제 QEMU로 라이브 복원 배선(계획 68 설계 §11.2)과 §4의 탐침(갈아타기 뒤 노드 이름·NBD 재연결)을 본다.
//! `cargo test -p orbivisor-driver --features qemu-it --test qemu_live_restore -- --nocapture` (탐침 값은 "PROBE" 줄)
//! VM(OS 없음)의 디스크 0은 우리 NBD 서버(저장소 헬퍼와 같은 서버, 메모리 이미지)를, 디스크 1은 옛 풀 백업처럼 qcow2
//! 체인(자식 → 전체)을 백업으로 읽는다. 게스트처럼 HMP `qemu-io`로 쓰고 읽는다.
#![cfg(feature = "qemu-it")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use orbivisor_driver::{
    Hypervisor, LiveRestoreDisk, LiveRestoreSource, QemuConfig, QemuHypervisor, StopMode,
    create_overlay,
};
use orbivisor_qemu::HostProfile;
use orbivisor_store::nbd::NbdServer;
use orbivisor_store::nbd::export::MemoryExport;
use orbivisor_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use orbivisor_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use uuid::Uuid;

const MIB: u64 = 1 << 20;
const SIZE: u64 = 8 * MIB;

fn short_tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

fn config(root: &Path) -> QemuConfig {
    let share = orbivisor_types::legacy::env("QEMU_SHARE")
        .unwrap_or_else(|| "/opt/homebrew/share/qemu".into());
    QemuConfig {
        host: HostProfile::native(Path::new(&share)),
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        systemd_scope: false,
        swtpm: Default::default(),
    }
}

fn disk(path: PathBuf, format: ImageFormat) -> RuntimeDisk {
    RuntimeDisk {
        path,
        kind: DiskKind::File,
        rbd: None,
        format,
        bus: DiskBus::VirtioBlk,
        boot_order: None,
    }
}

fn spec(disks: Vec<RuntimeDisk>) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "lr-vm".into(),
        cpu: CpuSpec {
            emulator: None,
            policy: Default::default(),
            thread_policy: None,
            flags: Vec::new(),
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
            max_vcpus: None,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks,
        nics: vec![],
        guest_agent: false,
        hotplug: None,
        cdroms: vec![],
        cloud_init: None,
        tpm: None,
        secure_boot: false,
        freeze_fs: false,
        firmware_epoch: 0,
        keep_tpm: false,
        cpu_resolved: None,
        numa: None,
        host_placement: None,
        pinning: None,
    }
}

fn tool(name: &str, args: &[&str]) {
    let out = std::process::Command::new(name)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{name} {args:?}: {out:?}");
}

fn raw_of(path: &Path, format: &str) -> Vec<u8> {
    let out = path.with_extension("check");
    tool(
        "qemu-img",
        &[
            "convert",
            "-U",
            "-f",
            format,
            "-O",
            "raw",
            path.to_str().unwrap(),
            out.to_str().unwrap(),
        ],
    );
    let b = std::fs::read(&out).unwrap();
    let _ = std::fs::remove_file(out);
    b
}

fn fill(image: &mut [u8], off: u64, len: u64, byte: u8) {
    image[off as usize..(off + len) as usize].fill(byte);
}

async fn guest_io(hv: &QemuHypervisor, id: Uuid, disk: usize, cmd: &str) -> String {
    let line = format!("qemu-io -d /machine/peripheral/disk{disk}-dev/virtio-backend \"{cmd}\"");
    hv.human_monitor_command(id, &line).await.unwrap()
}

async fn guest_write(hv: &QemuHypervisor, id: Uuid, disk: usize, cmd: &str) {
    let out = guest_io(hv, id, disk, cmd).await;
    assert!(!out.contains("rror"), "{cmd}: {out}");
}

/// 소켓 `socket`에 메모리 이미지를 `disk0`으로 내보내는 NBD 서버(헬퍼와 같은 서버). 따로 런타임에서 돌려 drop하면
/// 연결까지 모두 끊긴다(헬퍼 프로세스가 죽은 것처럼).
struct Server(Option<tokio::runtime::Runtime>);

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            rt.shutdown_background();
        }
    }
}

fn serve(socket: &Path, image: Vec<u8>) -> Server {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let mut server = NbdServer::new();
    server.add("disk0", Arc::new(MemoryExport::new(image, BTreeMap::new())));
    let listener = {
        let _g = rt.enter();
        orbivisor_store::nbd::serve::bind(socket).unwrap()
    };
    rt.spawn(Arc::new(server).serve(listener, std::future::pending()));
    Server(Some(rt))
}

/// 라이브 복원 배선으로 켜고 게스트가 쓰는 동안 mirror로 옮긴 뒤 갈아탄다. 갈아탄 뒤 장치는 대상 노드(`disk{i}`)이고
/// 크기 바꾸기·비트맵이 그 노드로 되며, 대상 파일은 백업 + 게스트가 쓴 것과 같다. 도중 NBD 백업 서버를 죽였다 다시
/// 띄워도 게스트 읽기는 붙잡혔다 이어진다(재연결 탐침).
#[tokio::test]
async fn live_restore_boots_copies_reconnects_and_pivots() {
    let tmp = short_tempdir();
    let dir = tmp.path();
    let hv = QemuHypervisor::new(config(dir)).await.unwrap();

    // 디스크 0의 백업(저장소 백업 — NBD), 디스크 1의 백업(옛 풀 백업 — qcow2 체인)
    let mut image0 = vec![0u8; SIZE as usize];
    fill(&mut image0, 0, 2 * MIB, 0x11);
    fill(&mut image0, 6 * MIB, MIB, 0x12);
    let socket = dir.join("bk.sock");
    let server = serve(&socket, image0.clone());
    let base = dir.join("full.qcow2");
    let child = dir.join("incr.qcow2");
    for f in [&base, &child] {
        tool(
            "qemu-img",
            &["create", "-q", "-f", "qcow2", f.to_str().unwrap(), "8M"],
        );
    }
    tool(
        "qemu-io",
        &[
            "-f",
            "qcow2",
            "-c",
            "write -P 0x21 0 2M",
            base.to_str().unwrap(),
        ],
    );
    tool(
        "qemu-io",
        &[
            "-f",
            "qcow2",
            "-c",
            "write -P 0x22 1M 64k",
            child.to_str().unwrap(),
        ],
    );
    let mut image1 = vec![0u8; SIZE as usize];
    fill(&mut image1, 0, 2 * MIB, 0x21);
    fill(&mut image1, MIB, 64 * 1024, 0x22);

    // 빈 대상 볼륨(raw·qcow2)과 오버레이
    let t0 = dir.join("t0.raw");
    let t1 = dir.join("t1.qcow2");
    tool(
        "qemu-img",
        &["create", "-q", "-f", "raw", t0.to_str().unwrap(), "8M"],
    );
    tool(
        "qemu-img",
        &["create", "-q", "-f", "qcow2", t1.to_str().unwrap(), "8M"],
    );
    let vm = spec(vec![
        disk(t0.clone(), ImageFormat::Raw),
        disk(t1.clone(), ImageFormat::Qcow2),
    ]);
    let state = dir.join("state").join("vm").join(vm.id.to_string());
    let wiring = vec![
        LiveRestoreDisk {
            source: LiveRestoreSource::Nbd {
                socket: socket.clone(),
                export: "disk0".into(),
            },
            overlay: state.join("lr-disk0.qcow2"),
            pivoted: false,
        },
        LiveRestoreDisk {
            source: LiveRestoreSource::Qcow2 {
                chain: vec![child.clone(), base.clone()],
            },
            overlay: state.join("lr-disk1.qcow2"),
            pivoted: false,
        },
    ];
    for d in &wiring {
        create_overlay(Path::new("qemu-img"), &d.overlay, SIZE)
            .await
            .unwrap();
    }
    hv.set_live_restore(vm.id, Some(&wiring)).await.unwrap();
    assert!(hv.live_restore_wired(vm.id));
    let started = std::time::Instant::now();
    hv.start(&vm, None).await.unwrap();
    println!(
        "PROBE live restore boot (QEMU up and running): {:?}",
        started.elapsed()
    );

    // 게스트는 백업을 본다(오버레이 아래). mirror 전에 쓴 것은 오버레이로 간다.
    let out = guest_io(&hv, vm.id, 0, "read -P 0x12 6M 64k").await;
    assert!(!out.contains("rror") && !out.contains("mismatch"), "{out}");
    let out = guest_io(&hv, vm.id, 1, "read -P 0x22 1M 64k").await;
    assert!(!out.contains("rror") && !out.contains("mismatch"), "{out}");
    guest_write(&hv, vm.id, 0, "write -P 0x77 0 64k").await;
    fill(&mut image0, 0, 64 * 1024, 0x77);

    // 재연결 탐침: 백업 서버를 죽이고 1.5초 뒤 같은 소켓으로 다시 띄운다 — 그사이의 게스트 읽기는 붙잡혔다 이어진다.
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let reader = {
        let hv = &hv;
        let id = vm.id;
        async move {
            let at = std::time::Instant::now();
            let out = guest_io(hv, id, 0, "read -P 0x12 6M 4k").await;
            (out, at.elapsed())
        }
    };
    let restart = async {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        serve(&socket, {
            let mut i = vec![0u8; SIZE as usize];
            fill(&mut i, 0, 2 * MIB, 0x11);
            fill(&mut i, 6 * MIB, MIB, 0x12);
            i
        })
    };
    let ((out, waited), _server) = tokio::join!(reader, restart);
    println!(
        "PROBE NBD reconnect: a guest read during a 1.5 s backup outage waited {waited:?}: {out:?}"
    );
    assert!(!out.contains("rror") && !out.contains("mismatch"), "{out}");

    // mirror를 걸고 다 옮기기를 기다린다. 그사이 게스트 쓰기는 대상에도 함께 간다(write-blocking).
    let jobs = hv.start_live_restore(vm.id).await.unwrap();
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    guest_write(&hv, vm.id, 0, "write -P 0x88 1M 64k").await;
    fill(&mut image0, MIB, 64 * 1024, 0x88);
    guest_write(&hv, vm.id, 1, "write -P 0x33 3M 64k").await;
    fill(&mut image1, 3 * MIB, 64 * 1024, 0x33);
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    loop {
        let jobs = hv.start_live_restore(vm.id).await.unwrap();
        assert!(jobs.iter().all(|j| j.error.is_none()), "{jobs:?}");
        if jobs.iter().all(|j| j.ready) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{jobs:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    hv.pivot_live_restore(vm.id).await.unwrap();
    // 다시 불러도 할 일이 없다(갈아탔다).
    hv.pivot_live_restore(vm.id).await.unwrap();
    let jobs = hv.start_live_restore(vm.id).await.unwrap();
    assert!(
        jobs.iter().all(|j| j.ready && j.error.is_none()),
        "{jobs:?}"
    );
    let info = hv.human_monitor_command(vm.id, "info block").await.unwrap();
    println!("PROBE after pivot `info block`:\n{info}");
    assert!(!info.contains("lr-disk"), "{info}");
    // 갈아탄 뒤 쓰기는 대상에만 — 백업 서버를 멈춰도 된다.
    drop(_server);
    guest_write(&hv, vm.id, 0, "write -P 0x44 5M 64k").await;
    fill(&mut image0, 5 * MIB, 64 * 1024, 0x44);
    // 지금 코드가 쓰는 노드 이름(`disk{i}`)으로 비트맵·크기 바꾸기가 된다.
    hv.add_bitmaps(vm.id, "opl-after-pivot").await.unwrap();
    let bitmaps = hv.backup_bitmaps(vm.id).await.unwrap();
    println!("PROBE bitmaps after pivot: {bitmaps:?}");
    assert_eq!(bitmaps.len(), 2, "{bitmaps:?}");
    hv.block_resize(vm.id, 1, 16 * MIB).await.unwrap();

    hv.set_live_restore(vm.id, None).await.unwrap();
    assert!(!hv.live_restore_wired(vm.id));
    assert!(!wiring[0].overlay.exists() && !wiring[1].overlay.exists());
    hv.stop(vm.id, StopMode::Force).await.unwrap();
    for _ in 0..100 {
        if hv.inspect(vm.id).await.unwrap().is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(raw_of(&t0, "raw"), image0);
    let mut got1 = raw_of(&t1, "qcow2");
    assert_eq!(got1.len() as u64, 16 * MIB);
    got1.truncate(SIZE as usize);
    assert_eq!(got1, image1);
}
