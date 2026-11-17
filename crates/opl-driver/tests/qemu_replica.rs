//! 실제 QEMU로 로컬 디스크 복제(계획 55)를 하는 드라이버 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it --test qemu_replica`
//! 원본 노드 a의 VM(OS 없음)이 노드 b의 받는 QEMU(`-machine none`)에 TLS NBD로 디스크를 보낸다. 처음은 전체를
//! 복제본에 바로, 다음은 기준 비트맵 뒤 바뀐 블록만 복제본을 backing으로 둔 겹층에 쓰고 합친다. 쓰기는 HMP
//! `qemu-io`로 한다. 인증서는 클러스터 PKI와 같은 모양으로 rcgen이 만든다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_driver::{
    DirStorage, DiskLocation, DriverError, Hypervisor, QemuConfig, QemuHypervisor, ReceiverExport,
    ReplicaDiskTarget, ReplicateRequest, StopMode, Storage, Storages, migration_tls_dir,
    write_migration_tls,
};
use opl_qemu::HostProfile;
use opl_types::replication::replica_bitmap;
use opl_types::resource::Resource;
use opl_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use uuid::Uuid;

/// 받는 노드의 주소. 노드 인증서의 IP SAN이다.
const HOST: &str = "127.0.0.1";
const MIB: u64 = 1 << 20;
const DISK_BYTES: u64 = 64 * MIB;
/// 받는 QEMU가 고를 포트의 시작(마이그레이션 시험과 겹치지 않게). 시험마다 따로 10개다 — 두 하이퍼바이저가
/// 같은 포트를 고르는 것은 하이퍼바이저 하나의 포트 잠금이 막지만, 함께 도는 시험은 하이퍼바이저가 다르다.
const PORT_BASE: u16 = 61200;
/// 보낸 양이 늘지 않으면 회차를 그만두는 한도
const STALL: Duration = Duration::from_secs(60);

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

fn cluster_ca() -> (String, Issuer<'static, KeyPair>) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params
        .distinguished_name
        .push(DnType::CommonName, "Orbivisor cluster it CA");
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let pem = params.self_signed(&key).unwrap().pem();
    (pem, Issuer::new(params, key))
}

fn node_cert(ca: &Issuer<'static, KeyPair>) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let sans = vec![HOST.to_string(), "localhost".to_string()];
    let mut params = CertificateParams::new(sans).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, Uuid::now_v7().to_string());
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, ca).unwrap();
    (cert.pem(), key.serialize_pem())
}

/// 한 호스트 위의 두 노드(실행·상태 디렉터리와 노드 인증서가 따로)와 노드 b의 dir 풀
struct Nodes {
    tmp: tempfile::TempDir,
    src: QemuHypervisor,
    dst: QemuHypervisor,
    pool: StoragePool,
    storage: Storages,
    ports: std::ops::RangeInclusive<u16>,
}

/// `slot`번째 시험의 두 노드
async fn nodes(slot: u16) -> Nodes {
    let tmp = short_tempdir();
    let (ca_pem, ca) = cluster_ca();
    let mut hvs = Vec::new();
    for name in ["a", "b"] {
        let cfg = config(&tmp.path().join(name));
        let (cert, key) = node_cert(&ca);
        write_migration_tls(&migration_tls_dir(&cfg.state_dir), &ca_pem, &cert, &key).unwrap();
        hvs.push(QemuHypervisor::new(cfg).await.unwrap());
    }
    let pool_dir = tmp.path().join("b").join("pool");
    std::fs::create_dir_all(&pool_dir).unwrap();
    let pool = Resource::new(
        "b",
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: pool_dir },
            nodes: vec![],
            shared: false,
            backups: false,
        },
    );
    let storage = Storages::new(
        "qemu-img",
        Arc::new(DirStorage::new("qemu-img", "/nonexistent/opl-pools")),
    );
    let dst = hvs.pop().unwrap();
    let src = hvs.pop().unwrap();
    Nodes {
        tmp,
        src,
        dst,
        pool,
        storage,
        ports: PORT_BASE + slot * 10..=PORT_BASE + slot * 10 + 9,
    }
}

/// qcow2 디스크 하나를 가진 UEFI VM(OS 없음)
fn spec(disk: PathBuf) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "repl-vm".into(),
        cpu: CpuSpec {
            sockets: 1,
            cores: 1,
            model: CpuModel::Host,
            max_vcpus: None,
        },
        memory_mib: 256,
        firmware: Firmware::Uefi,
        machine: Arch::native().machine(),
        disks: vec![RuntimeDisk {
            path: disk,
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![],
        guest_agent: false,
        hotplug: None,
        cdroms: vec![],
        cloud_init: None,
        tpm: None,
        secure_boot: false,
        freeze_fs: false,
    }
}

fn fmt(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    }
}

fn qemu_img(args: &[&str]) -> std::process::Output {
    std::process::Command::new("qemu-img")
        .args(args)
        .output()
        .unwrap()
}

/// 원본 노드 a의 디스크 파일을 만들고 무늬를 써 둔 뒤 VM을 켠다.
async fn running_vm(n: &Nodes) -> VmRuntimeSpec {
    let disk = n.tmp.path().join("a").join("disk.qcow2");
    std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
    let out = qemu_img(&["create", "-q", "-f", "qcow2", disk.to_str().unwrap(), "64M"]);
    assert!(out.status.success(), "{out:?}");
    let vm = spec(disk);
    n.src.start(&vm, None).await.unwrap();
    guest_write(&n.src, vm.id, "write -P 0x11 0 4M").await;
    vm
}

/// 실행 중인 VM의 첫 디스크에 게스트가 쓴 것처럼 쓴다(HMP `qemu-io`, 게스트 OS 없음).
async fn guest_write(hv: &QemuHypervisor, id: Uuid, write: &str) {
    let line = format!("qemu-io -d /machine/peripheral/disk0-dev/virtio-backend \"{write}\"");
    let out = hv.human_monitor_command(id, &line).await.unwrap();
    assert!(!out.contains("rror"), "{line}: {out}");
}

/// `qemu-io` 명령 하나를 꺼진 이미지에 실행한다. `read -P`는 무늬가 다르면 실패한다.
fn image_io(path: &Path, format: ImageFormat, command: &str) -> bool {
    let out = std::process::Command::new("qemu-io")
        .args(["-f", fmt(format), "-c", command, path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    out.status.success() && !text.contains("Pattern verification failed")
}

/// 노드 b의 복제본 볼륨 하나
struct Replica {
    id: Uuid,
    format: ImageFormat,
    path: PathBuf,
}

async fn create_replica(n: &Nodes, format: ImageFormat) -> Replica {
    let id = Uuid::now_v7();
    let path = n
        .storage
        .create_volume(&n.pool, id, DISK_BYTES, format)
        .await
        .unwrap();
    Replica { id, format, path }
}

fn overlay_path(n: &Nodes, run: Uuid, replica: &Replica) -> PathBuf {
    let dir = n.tmp.path().join("b").join("overlays");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{run}-{}.qcow2", replica.id))
}

/// 받는 QEMU를 띄우고(겹층이 있으면 겹층을 내보냄) 원본이 그곳에 보낸다. (보낸 바이트 또는 오류, 받는 QEMU 포트)
async fn round(
    n: &Nodes,
    vm: &VmRuntimeSpec,
    replica: &Replica,
    run: Uuid,
    overlay: Option<PathBuf>,
    base: Option<String>,
    target_node: Uuid,
) -> Result<u64, DriverError> {
    let export = ReceiverExport {
        name: replica.id.simple().to_string(),
        source: DiskLocation::Path(replica.path.clone(), DiskKind::File),
        format: replica.format,
        overlay,
    };
    let port = n
        .dst
        .start_replica_receiver(run, &[export], HOST, n.ports.clone())
        .await
        .unwrap();
    assert!(n.ports.contains(&port));
    assert_eq!(n.dst.replica_receivers().await.unwrap(), [run]);
    let req = ReplicateRequest {
        targets: vec![ReplicaDiskTarget {
            disk: 0,
            host: HOST.into(),
            port,
            export: replica.id.simple().to_string(),
            tls_hostname: HOST.into(),
            base,
        }],
        bitmap: replica_bitmap(target_node, run),
        speed: None,
        stall: STALL,
        progress: None,
        quiesce: false,
    };
    let sent = n.src.replicate_disks(vm.id, &req).await.map(|(b, _)| b);
    if sent.is_ok() {
        n.dst.finish_replica_receiver(run).await.unwrap();
        assert!(n.dst.replica_receivers().await.unwrap().is_empty());
    }
    sent
}

/// 첫 디스크의 비트맵 이름(정렬 — QEMU는 새것부터 알린다)
fn bitmap_names(disks: &[opl_driver::DiskBitmaps]) -> Vec<String> {
    let mut names: Vec<String> = disks[0].bitmaps.iter().map(|b| b.name.clone()).collect();
    names.sort();
    names
}

/// 원본을 끄고(이미지에 다 씀) 복제본과 견준다.
fn same_as_source(vm: &VmRuntimeSpec, replica: &Replica) -> bool {
    let out = qemu_img(&[
        "compare",
        "-q",
        "-f",
        "qcow2",
        "-F",
        fmt(replica.format),
        vm.disks[0].path.to_str().unwrap(),
        replica.path.to_str().unwrap(),
    ]);
    out.status.success()
}

/// 전체 → 쓰기 → 증분(겹층에 바뀐 블록만) → 합치기를 거친 복제본이 원본과 같다.
async fn replicate_twice(replica_format: ImageFormat, slot: u16) {
    let n = nodes(slot).await;
    let vm = running_vm(&n).await;
    let replica = create_replica(&n, replica_format).await;
    let target = Uuid::now_v7();

    // 처음은 전체를 복제본에 바로 쓴다. 디스크 크기만큼 센다(0인 곳도 write-zeroes로).
    let run1 = Uuid::now_v7();
    let full = round(&n, &vm, &replica, run1, None, None, target)
        .await
        .unwrap();
    assert!(full >= 4 * MIB, "{full}");
    let first = replica_bitmap(target, run1);
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    assert_eq!(bitmap_names(&bitmaps), std::slice::from_ref(&first));
    assert!(bitmaps[0].bitmaps[0].persistent);

    // 그 뒤 바뀐 64 KiB만 겹층으로 간다.
    guest_write(&n.src, vm.id, "write -P 0x22 8M 64k").await;
    let counted = n.src.backup_bitmaps(vm.id).await.unwrap()[0].bitmaps[0].count;
    assert_eq!(counted, 64 * 1024);
    let run2 = Uuid::now_v7();
    let overlay = overlay_path(&n, run2, &replica);
    n.storage
        .create_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    let increment = round(
        &n,
        &vm,
        &replica,
        run2,
        Some(overlay.clone()),
        Some(first.clone()),
        target,
    )
    .await
    .unwrap();
    assert!(increment > 0 && increment <= MIB, "{increment}");
    // 복제본은 아직 앞 회차 그대로다. 겹층을 합치면 들어간다.
    assert!(image_io(
        &replica.path,
        replica.format,
        "read -P 0x00 8M 64k"
    ));
    n.storage
        .commit_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    assert!(!overlay.exists());

    // 기준 비트맵은 드라이버가 지우지 않는다(호출자가 고른다).
    let second = replica_bitmap(target, run2);
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    let mut both = vec![first.clone(), second.clone()];
    both.sort();
    assert_eq!(bitmap_names(&bitmaps), both);
    n.src.remove_named_bitmaps(vm.id, &[first]).await.unwrap();
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    assert_eq!(bitmap_names(&bitmaps), [second]);

    n.src.stop(vm.id, StopMode::Force).await.unwrap();
    assert!(same_as_source(&vm, &replica));
    assert!(image_io(
        &replica.path,
        replica.format,
        "read -P 0x22 8M 64k"
    ));
    assert!(image_io(&replica.path, replica.format, "read -P 0x11 0 4M"));
}

#[tokio::test]
async fn replication_sends_only_changed_blocks() {
    replicate_twice(ImageFormat::Qcow2, 0).await;
}

/// raw 복제본(LVM-thin·ZFS와 같은 형식)도 qcow2 겹층에 받고 합친다.
#[tokio::test]
async fn raw_replicas_take_an_overlay_too() {
    replicate_twice(ImageFormat::Raw, 1).await;
}

/// 받는 QEMU가 사라진 증분 회차는 실패한다: 원본에 새 비트맵이 없고 기준 비트맵은 남으며, 겹층을 버리면
/// 복제본은 앞 회차 그대로다.
#[tokio::test]
async fn a_failed_increment_leaves_the_replica_as_it_was() {
    let n = nodes(2).await;
    let vm = running_vm(&n).await;
    let replica = create_replica(&n, ImageFormat::Qcow2).await;
    let target = Uuid::now_v7();
    let run1 = Uuid::now_v7();
    round(&n, &vm, &replica, run1, None, None, target)
        .await
        .unwrap();
    let first = replica_bitmap(target, run1);

    guest_write(&n.src, vm.id, "write -P 0x33 0 64k").await;
    let run2 = Uuid::now_v7();
    let overlay = overlay_path(&n, run2, &replica);
    n.storage
        .create_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    let export = ReceiverExport {
        name: replica.id.simple().to_string(),
        source: DiskLocation::Path(replica.path.clone(), DiskKind::File),
        format: replica.format,
        overlay: Some(overlay.clone()),
    };
    let port = n
        .dst
        .start_replica_receiver(run2, &[export], HOST, n.ports.clone())
        .await
        .unwrap();
    // 받는 노드가 회차를 버렸다(받는 QEMU가 죽음).
    n.dst.stop_replica_receiver(run2).await.unwrap();
    assert!(n.dst.replica_receivers().await.unwrap().is_empty());
    let req = ReplicateRequest {
        targets: vec![ReplicaDiskTarget {
            disk: 0,
            host: HOST.into(),
            port,
            export: replica.id.simple().to_string(),
            tls_hostname: HOST.into(),
            base: Some(first.clone()),
        }],
        bitmap: replica_bitmap(target, run2),
        speed: None,
        stall: STALL,
        progress: None,
        quiesce: false,
    };
    assert!(n.src.replicate_disks(vm.id, &req).await.is_err());
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    assert_eq!(bitmap_names(&bitmaps), std::slice::from_ref(&first));
    // 받는 QEMU가 없으니 끝낼 수 없다.
    assert!(n.dst.finish_replica_receiver(run2).await.is_err());

    std::fs::remove_file(&overlay).unwrap();
    assert!(image_io(
        &replica.path,
        replica.format,
        "read -P 0x11 0 64k"
    ));

    // 같은 기준으로 다시 하면 된다(기준 비트맵이 그 뒤 바뀐 것을 모두 담는다).
    let run3 = Uuid::now_v7();
    let overlay = overlay_path(&n, run3, &replica);
    n.storage
        .create_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    round(
        &n,
        &vm,
        &replica,
        run3,
        Some(overlay.clone()),
        Some(first),
        target,
    )
    .await
    .unwrap();
    n.storage
        .commit_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
    assert!(same_as_source(&vm, &replica));
}

/// 회차 도중 받는 QEMU가 죽어도(SIGKILL) 게스트 쓰기는 성공하고 회차만 실패한다: 원본은 플리싱으로 회차 시점의
/// 디스크를 보내므로 원격 쓰기 실패가 게스트 쓰기에 닿지 않는다. 플리싱 이미지는 남지 않고, 기준 비트맵이 남아
/// 다음 증분 회차가 원본과 같은 복제본을 만든다.
#[tokio::test]
async fn a_receiver_killed_mid_run_does_not_fail_guest_writes() {
    let n = nodes(3).await;
    let vm = running_vm(&n).await;
    let replica = create_replica(&n, ImageFormat::Qcow2).await;
    let target = Uuid::now_v7();
    let run1 = Uuid::now_v7();
    round(&n, &vm, &replica, run1, None, None, target)
        .await
        .unwrap();
    let first = replica_bitmap(target, run1);

    // 바뀐 16 MiB를 초당 1 MiB로 보내는 동안 받는 QEMU를 죽인다.
    guest_write(&n.src, vm.id, "write -P 0x33 16M 16M").await;
    let run2 = Uuid::now_v7();
    let overlay = overlay_path(&n, run2, &replica);
    n.storage
        .create_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    let export = ReceiverExport {
        name: replica.id.simple().to_string(),
        source: DiskLocation::Path(replica.path.clone(), DiskKind::File),
        format: replica.format,
        overlay: Some(overlay.clone()),
    };
    let port = n
        .dst
        .start_replica_receiver(run2, &[export], HOST, n.ports.clone())
        .await
        .unwrap();
    let req = ReplicateRequest {
        targets: vec![ReplicaDiskTarget {
            disk: 0,
            host: HOST.into(),
            port,
            export: replica.id.simple().to_string(),
            tls_hostname: HOST.into(),
            base: Some(first.clone()),
        }],
        bitmap: replica_bitmap(target, run2),
        speed: Some(MIB),
        stall: STALL,
        progress: None,
        quiesce: false,
    };
    let killing = async {
        tokio::time::sleep(Duration::from_secs(2)).await;
        n.dst.stop_replica_receiver(run2).await.unwrap();
        // 아직 보내지 않은 바뀐 블록에 게스트가 쓴다(플리싱이 없으면 백업 작업의 필터가 그 블록을 먼저 받는 쪽에
        // 쓰려다 실패해 게스트 쓰기가 실패한다).
        guest_write(&n.src, vm.id, "write -P 0x44 31M 64k").await;
    };
    let (sent, ()) = tokio::join!(n.src.replicate_disks(vm.id, &req), killing);
    assert!(sent.is_err(), "{sent:?}");
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    assert_eq!(bitmap_names(&bitmaps), std::slice::from_ref(&first));
    let state = n
        .tmp
        .path()
        .join("a")
        .join("state")
        .join("vm")
        .join(vm.id.to_string());
    let left: Vec<_> = std::fs::read_dir(&state)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("fleece-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_file(&overlay).unwrap();

    // 같은 기준으로 다시 하면 된다.
    let run3 = Uuid::now_v7();
    let overlay = overlay_path(&n, run3, &replica);
    n.storage
        .create_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    round(
        &n,
        &vm,
        &replica,
        run3,
        Some(overlay.clone()),
        Some(first),
        target,
    )
    .await
    .unwrap();
    n.storage
        .commit_overlay(&n.pool, replica.id, replica.format, &overlay)
        .await
        .unwrap();
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
    assert!(image_io(
        &vm.disks[0].path,
        ImageFormat::Qcow2,
        "read -P 0x44 31M 64k"
    ));
    assert!(same_as_source(&vm, &replica));
}
