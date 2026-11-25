//! 실제 QEMU로 저장소 백업의 읽기(계획 68 설계 §8.1·§8.5)를 보는 드라이버 통합 테스트와 §4의 탐침.
//! `cargo test -p orbivisor-driver --features qemu-it --test qemu_fleece -- --nocapture` (탐침 값은 "PROBE" 줄)
//! VM(OS 없음)의 디스크에 HMP `qemu-io`로 게스트처럼 쓰고, 플리싱 내보내기를 orbivisor-store의 NBD 클라이언트로 읽는다.
//! - 시점 내용: 시점 뒤에 쓴 것이 내보내기에 보이지 않는다(qcow2·raw 두 디스크가 한 시점).
//! - 남의 노드 비트맵: 디스크 노드의 비트맵(부모 시점까지 얼린 임시 비트맵)을 snapshot-access 내보내기에 단다.
//! - 끝: 성공은 부모 비트맵을 지우고 새 비트맵만, 실패는 새 비트맵을 지우고 부모는 켜진 채.
//! - 지킴이: 남은 공간이 한도보다 적으면 내보내기와 플리싱을 떼고 게스트는 계속 쓴다.
//! - ENOSPC: 작은 파일 시스템(macOS `hdiutil`, Linux root면 tmpfs)의 플리싱이 차면 게스트 쓰기는 성공하고 읽기만 실패.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbivisor_driver::{
    DriverError, FleeceExport, FleeceRequest, Hypervisor, QemuConfig, QemuHypervisor, StopMode,
};
use orbivisor_qemu::HostProfile;
use orbivisor_store::nbd::{BASE_ALLOCATION, NbdClient, STATE_DIRTY, STATE_ZERO};
use orbivisor_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use orbivisor_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn short_tempdir() -> tempfile::TempDir {
    // macOS의 기본 TMPDIR은 소켓 경로 한도를 넘기므로 /tmp를 쓴다.
    tempfile::Builder::new()
        .prefix("opl")
        .tempdir_in("/tmp")
        .unwrap()
}

fn config(root: &Path) -> QemuConfig {
    let share =
        std::env::var("OPL_QEMU_SHARE").unwrap_or_else(|_| "/opt/homebrew/share/qemu".into());
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

/// UEFI VM(OS 없음)
fn spec(disks: Vec<RuntimeDisk>) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "fleece-vm".into(),
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

fn qemu_img(args: &[&str]) {
    let out = std::process::Command::new("qemu-img")
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

fn image(dir: &Path, name: &str, format: ImageFormat, size: &str) -> PathBuf {
    let p = dir.join(name);
    let f = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    qemu_img(&["create", "-q", "-f", f, p.to_str().unwrap(), size]);
    p
}

/// 실행 중인 VM의 `disk`번째 디스크에 게스트가 쓴 것처럼 쓴다(HMP `qemu-io`, 게스트 OS 없음). 장치 경로로 쓰므로
/// 플리싱 필터가 장치에 끼워져 있어도 그 필터를 거친다.
async fn guest_io(hv: &QemuHypervisor, id: Uuid, disk: usize, cmd: &str) -> String {
    let line = format!("qemu-io -d /machine/peripheral/disk{disk}-dev/virtio-backend \"{cmd}\"");
    hv.human_monitor_command(id, &line).await.unwrap()
}

async fn guest_write(hv: &QemuHypervisor, id: Uuid, disk: usize, cmd: &str) {
    let out = guest_io(hv, id, disk, cmd).await;
    assert!(!out.contains("rror"), "{cmd}: {out}");
}

fn request(parent: Option<&str>, bitmap: &str) -> FleeceRequest {
    FleeceRequest {
        run: Uuid::now_v7(),
        parent: parent.map(str::to_string),
        bitmap: bitmap.into(),
        quiesce: false,
        reserve_min: 0,
        min_free: 0,
    }
}

/// 디스크마다 비트맵 (이름, 따라가는가)
async fn bitmaps(hv: &QemuHypervisor, id: Uuid) -> Vec<Vec<(String, bool)>> {
    hv.backup_bitmaps(id)
        .await
        .unwrap()
        .into_iter()
        .map(|d| {
            let mut v: Vec<(String, bool)> = d
                .bitmaps
                .into_iter()
                .map(|b| (b.name, b.recording))
                .collect();
            v.sort();
            v
        })
        .collect()
}

async fn client(ex: &FleeceExport, disk: usize) -> NbdClient {
    let d = &ex.disks[disk];
    let mut contexts = vec![BASE_ALLOCATION];
    if let Some(c) = &d.dirty_context {
        contexts.push(c);
    }
    NbdClient::connect(&ex.socket, &d.export, &contexts)
        .await
        .unwrap()
}

/// 범위들 가운데 `keep`인 것의 (시작, 끝)
fn spans(ext: &[orbivisor_store::nbd::Extent], keep: impl Fn(u32) -> bool) -> Vec<(u64, u64)> {
    let mut out: Vec<(u64, u64)> = Vec::new();
    let mut at = 0;
    for e in ext {
        if keep(e.flags) {
            match out.last_mut() {
                Some(last) if last.1 == at => last.1 += e.length,
                _ => out.push((at, at + e.length)),
            }
        }
        at += e.length;
    }
    out
}

/// 시점 내용·두 디스크 한 시점·부모 비트맵 문맥·끝의 비트맵 규칙
#[tokio::test]
async fn fleecing_exports_the_point_in_time_with_the_parent_bitmap() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let d0 = image(tmp.path(), "d0.qcow2", ImageFormat::Qcow2, "64M");
    let d1 = image(tmp.path(), "d1.raw", ImageFormat::Raw, "32M");
    let vm = spec(vec![
        disk(d0, ImageFormat::Qcow2),
        disk(d1, ImageFormat::Raw),
    ]);
    hv.start(&vm, None).await.unwrap();
    let id = vm.id;
    guest_write(&hv, id, 0, "write -P 0x11 0 4M").await;
    guest_write(&hv, id, 1, "write -P 0x12 0 1M").await;
    hv.add_bitmaps(id, "opl-par").await.unwrap();
    // 부모 뒤에 바뀐 곳
    guest_write(&hv, id, 0, "write -P 0x22 8M 64k").await;
    guest_write(&hv, id, 1, "write -P 0x23 2M 64k").await;

    let req = request(Some("opl-par"), "opl-new");
    let ex = hv.fleece_begin(id, &req).await.unwrap();
    assert_eq!(ex.disks.len(), 2);
    assert_eq!(ex.disks[0].size, 64 * MIB);
    assert_eq!(ex.disks[1].size, 32 * MIB);
    // 시점 뒤의 쓰기(옛 내용 덮기·새 곳)
    guest_write(&hv, id, 0, "write -P 0x33 0 64k").await;
    guest_write(&hv, id, 0, "write -P 0x44 8M 64k").await;
    guest_write(&hv, id, 0, "write -P 0x55 16M 64k").await;
    guest_write(&hv, id, 1, "write -P 0x66 0 64k").await;

    let mut c0 = client(&ex, 0).await;
    assert_eq!(c0.size(), 64 * MIB);
    assert_eq!(c0.read(0, 65536).await.unwrap(), vec![0x11; 65536]);
    assert_eq!(c0.read(8 * MIB, 65536).await.unwrap(), vec![0x22; 65536]);
    assert_eq!(c0.read(16 * MIB, 65536).await.unwrap(), vec![0; 65536]);
    let mut c1 = client(&ex, 1).await;
    assert_eq!(c1.read(0, 65536).await.unwrap(), vec![0x12; 65536]);
    assert_eq!(c1.read(2 * MIB, 65536).await.unwrap(), vec![0x23; 65536]);
    // 부모 문맥: 부모 시점 → 이 시점에 바뀐 곳만(시점 뒤 쓰기는 없음)
    let ctx = ex.disks[0].dirty_context.clone().unwrap();
    let dirty0 = c0.extents(&ctx, 0, 64 * MIB).await.unwrap();
    let dirty1 = c1.extents(&ctx, 0, 32 * MIB).await.unwrap();
    eprintln!("PROBE dirty extents disk0 {dirty0:?} disk1 {dirty1:?}");
    assert_eq!(
        spans(&dirty0, |f| f & STATE_DIRTY != 0),
        vec![(8 * MIB, 8 * MIB + 65536)]
    );
    assert_eq!(
        spans(&dirty1, |f| f & STATE_DIRTY != 0),
        vec![(2 * MIB, 2 * MIB + 65536)]
    );
    let alloc = c0.extents(BASE_ALLOCATION, 0, 64 * MIB).await.unwrap();
    eprintln!("PROBE base:allocation of the fleece export disk0 {alloc:?}");
    let data = spans(&alloc, |f| f & STATE_ZERO == 0);
    assert!(
        data.contains(&(0, 4 * MIB)) || data.first() == Some(&(0, 4 * MIB)),
        "{data:?}"
    );
    // 읽는 동안: 부모는 켜진 채, 새 비트맵은 따라가고, 임시 비트맵은 꺼져 있다.
    assert_eq!(
        bitmaps(&hv, id).await[0],
        [
            ("opl-fl-point".to_string(), false),
            ("opl-new".to_string(), true),
            ("opl-par".to_string(), true)
        ]
    );
    hv.fleece_end(id, &req, true).await.unwrap();
    assert!(!ex.socket.exists());
    assert!(c0.read(0, 512).await.is_err());
    for disk in bitmaps(&hv, id).await {
        assert_eq!(disk, [("opl-new".to_string(), true)]);
    }
    // 장치는 디스크 노드로 돌아왔다(쓴 것이 이미지에 있다).
    let out = guest_io(&hv, id, 0, "read -P 0x55 16M 64k").await;
    assert!(!out.contains("fail"), "{out}");

    // 다음 읽기가 실패하면 새 비트맵만 지우고 부모(opl-new)는 켜진 채 남는다.
    let req2 = request(Some("opl-new"), "opl-next");
    let ex2 = hv.fleece_begin(id, &req2).await.unwrap();
    let mut c = client(&ex2, 0).await;
    let dirty = c.extents(&ctx, 0, 64 * MIB).await.unwrap();
    // opl-new 시점 뒤에 쓴 곳: 0, 8M, 16M의 64k
    assert_eq!(
        spans(&dirty, |f| f & STATE_DIRTY != 0),
        vec![
            (0, 65536),
            (8 * MIB, 8 * MIB + 65536),
            (16 * MIB, 16 * MIB + 65536)
        ]
    );
    hv.fleece_end(id, &req2, false).await.unwrap();
    for disk in bitmaps(&hv, id).await {
        assert_eq!(disk, [("opl-new".to_string(), true)]);
    }
    // 부모 없이(전체) — 문맥 없음
    let req3 = request(None, "opl-full");
    let ex3 = hv.fleece_begin(id, &req3).await.unwrap();
    assert!(ex3.disks.iter().all(|d| d.dirty_context.is_none()));
    // 없는 부모로 열면 아무것도 남기지 않고 실패한다.
    hv.fleece_end(id, &req3, true).await.unwrap();
    let bad = hv
        .fleece_begin(id, &request(Some("opl-none"), "opl-z"))
        .await;
    assert!(matches!(bad, Err(DriverError::Backup(_))), "{bad:?}");
    for disk in bitmaps(&hv, id).await {
        assert_eq!(
            disk,
            [
                ("opl-full".to_string(), true),
                ("opl-new".to_string(), true)
            ]
        );
    }
    hv.stop(id, StopMode::Force).await.unwrap();
}

/// 지킴이(설계 §8.5): 남은 공간이 한도보다 적으면 1초 안에 내보내기와 플리싱을 뗀다 — 읽기는 실패하고 게스트 쓰기는
/// 그대로 되며, 끝은 그 까닭으로 실패하고 새 비트맵을 지운다. 시작 전 공간 확인도 본다.
#[tokio::test]
async fn the_fleece_guard_stops_the_read_and_the_guest_keeps_writing() {
    let tmp = short_tempdir();
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let d0 = image(tmp.path(), "d0.qcow2", ImageFormat::Qcow2, "32M");
    let vm = spec(vec![disk(d0, ImageFormat::Qcow2)]);
    hv.start(&vm, None).await.unwrap();
    let id = vm.id;
    guest_write(&hv, id, 0, "write -P 0x11 0 1M").await;

    let mut too_big = request(None, "opl-a");
    too_big.reserve_min = u64::MAX;
    let e = hv.fleece_begin(id, &too_big).await.unwrap_err();
    assert!(e.to_string().contains("not enough scratch space"), "{e}");
    assert!(bitmaps(&hv, id).await[0].is_empty());

    let mut req = request(None, "opl-a");
    req.min_free = u64::MAX;
    let ex = hv.fleece_begin(id, &req).await.unwrap();
    let mut c = client(&ex, 0).await;
    let mut stopped = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if c.read(0, 4096).await.is_err() {
            stopped = true;
            break;
        }
    }
    assert!(stopped, "the guard did not stop the export");
    guest_write(&hv, id, 0, "write -P 0x22 0 1M").await;
    let e = hv.fleece_end(id, &req, true).await.unwrap_err();
    assert!(e.to_string().contains("the backup was stopped"), "{e}");
    assert!(bitmaps(&hv, id).await[0].is_empty());
    // 다음 읽기는 다시 된다.
    let ok = request(None, "opl-b");
    let ex = hv.fleece_begin(id, &ok).await.unwrap();
    let mut c = client(&ex, 0).await;
    assert_eq!(c.read(0, 4096).await.unwrap(), vec![0x22; 4096]);
    hv.fleece_end(id, &ok, true).await.unwrap();
    hv.stop(id, StopMode::Force).await.unwrap();
}

/// 작은 파일 시스템(플리싱 이미지 자리). 끝날 때 뗀다. 만들 수 없으면 `None`(Linux에서 root가 아님).
struct SmallFs {
    mount: PathBuf,
    image: Option<PathBuf>,
}

impl SmallFs {
    fn new(mount: &Path, mib: u64) -> Option<SmallFs> {
        std::fs::create_dir_all(mount).unwrap();
        if cfg!(target_os = "macos") {
            let image = mount.with_extension("dmg");
            let ok = std::process::Command::new("hdiutil")
                .args([
                    "create",
                    "-quiet",
                    "-size",
                    &format!("{mib}m"),
                    "-fs",
                    "APFS",
                ])
                .args(["-volname", "oplfleece", "-o", image.to_str().unwrap()])
                .status()
                .unwrap()
                .success()
                && std::process::Command::new("hdiutil")
                    .args(["attach", "-quiet", "-nobrowse", "-mountpoint"])
                    .args([mount.to_str().unwrap(), image.to_str().unwrap()])
                    .status()
                    .unwrap()
                    .success();
            ok.then(|| SmallFs {
                mount: mount.into(),
                image: Some(image),
            })
        } else if rustix::process::geteuid().is_root() {
            let ok = std::process::Command::new("mount")
                .args(["-t", "tmpfs", "-o", &format!("size={mib}m"), "tmpfs"])
                .arg(mount)
                .status()
                .unwrap()
                .success();
            ok.then(|| SmallFs {
                mount: mount.into(),
                image: None,
            })
        } else {
            None
        }
    }
}

impl Drop for SmallFs {
    fn drop(&mut self) {
        if self.image.is_some() {
            let _ = std::process::Command::new("hdiutil")
                .args(["detach", "-quiet", "-force"])
                .arg(&self.mount)
                .status();
        } else {
            let _ = std::process::Command::new("umount")
                .arg(&self.mount)
                .status();
        }
    }
}

/// 탐침(설계 §4 "fleece가 차면"): 플리싱 이미지 자리가 차면 copy-before-write 필터(`on-cbw-error: break-snapshot`)가
/// 게스트 쓰기를 그대로 하고 시점만 깬다 — 내보내기 읽기는 실패하고 게스트는 오류를 보지 않는다(지킴이는 끔).
#[tokio::test]
async fn a_full_fleece_breaks_the_read_not_the_guest() {
    let tmp = short_tempdir();
    // UEFI 변수(aarch64는 64 MiB)도 상태 디렉터리에 있다.
    let Some(_fs) = SmallFs::new(&tmp.path().join("state"), 160) else {
        eprintln!("PROBE fleece ENOSPC: skipped (needs root for tmpfs on Linux)");
        return;
    };
    let hv = QemuHypervisor::new(config(tmp.path())).await.unwrap();
    let d0 = image(tmp.path(), "d0.qcow2", ImageFormat::Qcow2, "256M");
    let vm = spec(vec![disk(d0, ImageFormat::Qcow2)]);
    let started = hv.start(&vm, None).await;
    let st = rustix::fs::statvfs(tmp.path().join("state")).unwrap();
    eprintln!(
        "PROBE small fs free after start: {}",
        st.f_bavail * st.f_frsize
    );
    started.unwrap();
    let id = vm.id;
    guest_write(&hv, id, 0, "write -P 0x11 0 200M").await;
    let req = request(None, "opl-a");
    let ex = hv.fleece_begin(id, &req).await.unwrap();
    let before = rustix::fs::statvfs(tmp.path().join("state")).unwrap();
    // 옛 내용 200 MiB를 플리싱에 옮겨야 하지만 자리가 모자라다.
    let out = guest_io(&hv, id, 0, "write -P 0x22 0 200M").await;
    let after = rustix::fs::statvfs(tmp.path().join("state")).unwrap();
    let mut c = client(&ex, 0).await;
    let tail = c.read(199 * MIB, 4096).await;
    eprintln!(
        "PROBE fleece ENOSPC: free {} → {} bytes; guest write: {}; read of the export after the fill: {:?}",
        before.f_bavail * before.f_frsize,
        after.f_bavail * after.f_frsize,
        out.trim().replace('\n', " | "),
        tail.as_ref().map(|d| d[0])
    );
    assert!(!out.contains("rror"), "the guest write failed: {out}");
    assert!(tail.is_err(), "the broken snapshot still reads");
    let end = hv.fleece_end(id, &req, false).await;
    eprintln!("PROBE fleece ENOSPC: fleece_end after the fill: {end:?}");
    end.unwrap();
    let back = guest_io(&hv, id, 0, "read -P 0x22 199M 4k").await;
    assert!(!back.contains("fail"), "{back}");
    hv.stop(id, StopMode::Force).await.unwrap();
}
