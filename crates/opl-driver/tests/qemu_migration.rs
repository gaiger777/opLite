//! 실제 QEMU 둘로 한 호스트에서 TLS 라이브 마이그레이션을 하는 드라이버 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it --test qemu_migration`
//! 두 하이퍼바이저는 실행·상태 디렉터리가 다른 두 노드를 흉내 내고, 디스크는 같은 파일(공유 풀)이다.
//! 로컬 디스크 시험은 노드마다 다른 파일이고, 원본이 대상의 NBD 내보내기로 미러한다.
//! 인증서는 클러스터 PKI(`opl-peer`의 `pki`)와 같은 모양으로 rcgen이 만든다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_driver::{
    DriverError, EventKind, Hypervisor, MigrateOptions, MigrateTarget, MirrorOptions,
    MirrorProgress, MirrorTarget, QemuConfig, QemuHypervisor, RuntimeStatus, StopMode,
    migration_tls_dir, write_migration_tls,
};
use opl_qemu::{Accel, HostProfile};
use opl_types::runtime::{DiskKind, ImageFormat, RuntimeDisk, VmRuntimeSpec};
use opl_types::vm::{Arch, CpuModel, CpuSpec, DiskBus, Firmware};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use uuid::Uuid;

/// 받는 노드의 주소. 노드 인증서의 IP SAN이다.
const HOST: &str = "127.0.0.1";

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
    let mut host = HostProfile::native(Path::new(&share));
    // HVF(macOS)에서는 보내는 QEMU가 메모리 변경 추적을 켜다 가끔 죽는다(QEMU 11.1, `assert_hvf_ok`
    // in `memory_global_dirty_log_start`). macOS 노드는 마이그레이션을 하지 않으므로(API가 거절)
    // 이 Mac에서는 TCG로 돌린다.
    if host.accel == Accel::Hvf {
        host.accel = Accel::Tcg;
    }
    QemuConfig {
        host,
        run_dir: root.join("run"),
        state_dir: root.join("state"),
        net: None,
        // systemd 스코프 이름은 VM id로 정해져(`opl-vm-{id}.scope`) 한 호스트의 두 QEMU가 겹친다.
        // 받는 쪽을 띄울 때 남은 스코프로 보고 원본 스코프를 멈추므로 쓰지 않는다.
        systemd_scope: false,
    }
}

/// 클러스터 CA(`ClusterCa::generate`와 같은 모양)
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

/// 노드 인증서와 키(`ClusterCa::sign_node`와 같은 모양: CN은 노드 id, 서버·클라이언트 겸용)
fn node_cert(ca: &Issuer<'static, KeyPair>, sans: &[&str]) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params =
        CertificateParams::new(sans.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
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

/// 한 호스트 위의 두 노드: 각자 실행·상태 디렉터리와 노드 인증서를 가진다.
struct Nodes {
    tmp: tempfile::TempDir,
    src: QemuHypervisor,
    dst: QemuHypervisor,
}

async fn nodes() -> Nodes {
    let tmp = short_tempdir();
    let (ca_pem, ca) = cluster_ca();
    let mut hvs = Vec::new();
    for name in ["a", "b"] {
        let cfg = config(&tmp.path().join(name));
        let (cert, key) = node_cert(&ca, &[HOST, "localhost"]);
        write_migration_tls(&migration_tls_dir(&cfg.state_dir), &ca_pem, &cert, &key).unwrap();
        hvs.push(QemuHypervisor::new(cfg).await.unwrap());
    }
    let dst = hvs.pop().unwrap();
    let src = hvs.pop().unwrap();
    Nodes { tmp, src, dst }
}

/// 공유 풀처럼 두 노드가 같은 경로로 여는 qcow2 디스크 하나를 가진 UEFI VM(OS 없음)
fn spec(disk: PathBuf) -> VmRuntimeSpec {
    VmRuntimeSpec {
        id: Uuid::now_v7(),
        name: "mig-vm".into(),
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
            kind: DiskKind::File,
            rbd: None,
            format: ImageFormat::Qcow2,
            bus: DiskBus::VirtioBlk,
            boot_order: Some(1),
        }],
        nics: vec![],
    }
}

fn create_disk(dir: &Path) -> PathBuf {
    let disk = dir.join("disk.qcow2");
    let status = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2"])
        .arg(&disk)
        .arg("1G")
        .status()
        .unwrap();
    assert!(status.success());
    disk
}

fn vars(root: &Path, id: Uuid) -> PathBuf {
    opl_driver::uefi_vars_path(&root.join("state"), id)
}

async fn status(hv: &QemuHypervisor, id: Uuid) -> Option<RuntimeStatus> {
    hv.inspect(id).await.unwrap().map(|s| s.status)
}

fn opts() -> MigrateOptions {
    MigrateOptions {
        bandwidth: None,
        timeout: Duration::from_secs(120),
        switchover_mirrors: false,
    }
}

#[tokio::test]
async fn migrates_a_running_vm_over_tls() {
    let n = nodes().await;
    let disk = create_disk(n.tmp.path());
    let vm = spec(disk.clone());
    let task = Uuid::now_v7();
    // 원본의 UEFI 변수 파일(템플릿 복사본) 끝에 표식을 남기고 띄운다. 대상은 템플릿으로 시작하므로,
    // 넘겨받은 뒤 대상 파일이 원본과 같으면 QEMU가 pflash 내용을 옮겨 쓴 것이다.
    let src_vars = vars(&n.tmp.path().join("a"), vm.id);
    let dst_vars = vars(&n.tmp.path().join("b"), vm.id);
    let mut content = std::fs::read(&config(n.tmp.path()).host.uefi_vars_template).unwrap();
    let marker = b"opl-migration-it";
    let at = content.len() - marker.len();
    content[at..].copy_from_slice(marker);
    std::fs::create_dir_all(src_vars.parent().unwrap()).unwrap();
    std::fs::write(&src_vars, &content).unwrap();
    n.src.start(&vm, None).await.unwrap();
    // 증분 백업의 비트맵도 넘어간다.
    n.src.add_bitmaps(vm.id, "opl-a").await.unwrap();

    // 같은 QEMU다.
    let version = n.src.running_qemu_version(vm.id).await.unwrap();
    assert_eq!(version, n.dst.qemu_version().await.unwrap());

    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61000..=61019, &[])
        .await
        .unwrap()
        .migration;
    assert!((61000..=61019).contains(&port));
    assert_eq!(
        status(&n.dst, vm.id).await,
        Some(RuntimeStatus::Other("inmigrate".into()))
    );
    assert_eq!(n.dst.list_incoming().await.unwrap(), vec![(vm.id, task)]);
    // 그 포트는 이제 받는 QEMU가 쓴다.
    assert!(std::net::TcpListener::bind((HOST, port)).is_err());

    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    n.src.migrate(vm.id, &target, &opts()).await.unwrap();
    assert_eq!(
        status(&n.src, vm.id).await,
        Some(RuntimeStatus::Other("postmigrate".into()))
    );
    n.dst.incoming_wait(vm.id).await.unwrap();
    // 받은 뒤에도 넘겨받기 전까지는 멈춰 있다.
    assert_eq!(status(&n.dst, vm.id).await, Some(RuntimeStatus::Paused));

    assert!(!std::fs::read(&dst_vars).unwrap().ends_with(marker));
    n.dst.finish_incoming(vm.id).await.unwrap();
    assert_eq!(status(&n.dst, vm.id).await, Some(RuntimeStatus::Running));
    let bitmaps = n.dst.backup_bitmaps(vm.id).await.unwrap();
    let a = &bitmaps[0].bitmaps;
    assert_eq!(a.len(), 1, "{bitmaps:?}");
    assert_eq!(
        (a[0].name.as_str(), a[0].recording, a[0].inconsistent),
        ("opl-a", true, false)
    );
    assert!(n.dst.list_incoming().await.unwrap().is_empty());
    // 대상 QEMU가 이어서 실행하면서 pflash(UEFI 변수)를 자기 파일에 원본 내용으로 쓴다. 대상
    // 펌웨어가 곧바로 변수를 더 바꿀 수 있으므로(KVM) 파일 전체가 아니라 원본 표식만 본다.
    let same_vars = async {
        loop {
            let dst = std::fs::read(&dst_vars).unwrap();
            if dst.ends_with(marker) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), same_vars)
        .await
        .is_err()
    {
        let (src, dst) = (
            std::fs::read(&src_vars).unwrap(),
            std::fs::read(&dst_vars).unwrap(),
        );
        let diff = src.iter().zip(&dst).position(|(a, b)| a != b);
        panic!(
            "UEFI variables were not written on the target: src {} bytes (marker {}), dst {} bytes (marker {}), first difference at {diff:?}",
            src.len(),
            src.ends_with(marker),
            dst.len(),
            dst.ends_with(marker)
        );
    }

    // 원본 정리: postmigrate QEMU를 끈다.
    assert!(n.src.stop_migrated(vm.id).await.unwrap());
    assert_eq!(status(&n.src, vm.id).await, None);
    assert!(!n.dst.stop_migrated(vm.id).await.unwrap());
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    // 대상이 끝날 때 이미지에 저장했다.
    let out = std::process::Command::new("qemu-img")
        .args(["info", "--output=json"])
        .arg(&disk)
        .output()
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        info["format-specific"]["data"]["bitmaps"][0]["name"], "opl-a",
        "{info}"
    );
}

/// 확정 전에 실패하면 원본은 그대로 돈다. 받는 QEMU는 버린다.
#[tokio::test]
async fn failed_migration_keeps_the_source_running() {
    let n = nodes().await;
    let vm = spec(create_disk(n.tmp.path()));
    let task = Uuid::now_v7();
    n.src.start(&vm, None).await.unwrap();

    // 아무도 받지 않는 포트
    let closed = {
        let l = std::net::TcpListener::bind((HOST, 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let target = MigrateTarget {
        host: HOST.into(),
        port: closed,
        tls_hostname: HOST.into(),
    };
    let err = n.src.migrate(vm.id, &target, &opts()).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));

    // 받는 노드 인증서에 없는 이름: TLS가 실패한다.
    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61020..=61039, &[])
        .await
        .unwrap()
        .migration;
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: "not-the-node".into(),
    };
    let err = n.src.migrate(vm.id, &target, &opts()).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));
    assert!(!n.src.stop_migrated(vm.id).await.unwrap());

    // 받는 쪽을 버린다(MigrateIncomingAbort가 할 일).
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(status(&n.dst, vm.id).await, None);
    assert!(n.dst.list_incoming().await.unwrap().is_empty());

    // 다시 받고 보내면 된다(원본 QEMU에 TLS 객체를 다시 만든다).
    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61020..=61039, &[])
        .await
        .unwrap()
        .migration;
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    n.src.migrate(vm.id, &target, &opts()).await.unwrap();
    n.dst.incoming_wait(vm.id).await.unwrap();

    // 확정하지 않고 되돌린다: 받은 QEMU를 끄고 원본을 이어서 실행한다.
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    n.src.resume(vm.id).await.unwrap();
    assert_eq!(status(&n.src, vm.id).await, Some(RuntimeStatus::Running));
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// 이미지에 저장된 dirty bitmap마다 (이름, 플래그). 읽기만 하므로 잠금 없이(`-U`) 연다.
fn image_bitmaps(disk: &Path) -> Vec<(String, Vec<String>)> {
    let out = std::process::Command::new("qemu-img")
        .args(["info", "-U", "--output=json"])
        .arg(disk)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    info["format-specific"]["data"]["bitmaps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            let flags = b["flags"].as_array().into_iter().flatten();
            (
                b["name"].as_str().unwrap().to_string(),
                flags.map(|f| f.as_str().unwrap().to_string()).collect(),
            )
        })
        .collect()
}

/// QEMU가 비정상으로 끝나 이미지에 inconsistent(in-use)로 남은 증분 백업 비트맵은 다시 켤 때 지운다.
/// 남으면 `block_resize`와 마이그레이션이 실패한다. qcow2의 비트맵은 공유 이미지로 넘어가므로 능력
/// `dirty-bitmaps`를 켜지 않고, 그래서 실패한 마이그레이션 뒤에도 비트맵이 in-use로 남지 않는다.
#[tokio::test]
async fn inconsistent_bitmaps_are_dropped_and_do_not_block_migration() {
    let n = nodes().await;
    let disk = create_disk(n.tmp.path());
    let vm = spec(disk.clone());
    n.src.start(&vm, None).await.unwrap();
    n.src.add_bitmaps(vm.id, "opl-a").await.unwrap();
    // 끝날 때 이미지에 저장한 비트맵을 다시 연(in-use로 표시한) 채로 QEMU가 죽는다.
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
    n.src.start(&vm, None).await.unwrap();
    let pid = n.src.inspect(vm.id).await.unwrap().unwrap().pid;
    let mut events = n.src.subscribe();
    let killed = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(killed.success());
    // 모니터가 실행 디렉터리를 치우고 알린다.
    let crashed = async {
        loop {
            let ev = events.recv().await.unwrap();
            if ev.id == vm.id && ev.kind == EventKind::Crashed {
                return;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), crashed)
        .await
        .unwrap();
    // Linux에서 pidfile 잠금(POSIX)은 fd를 닫을 때 바로 풀리지만 이미지 잠금(OFD)은 파일의 마지막
    // 참조가 사라질 때 풀려 Crashed 뒤에도 잠깐 남을 수 있다. 프로세스가 사라질 때까지 기다린다.
    tokio::time::timeout(Duration::from_secs(10), async {
        while process_exists(pid) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("killed QEMU did not exit within 10s");
    assert_eq!(
        image_bitmaps(&disk),
        [(
            "opl-a".to_string(),
            vec!["in-use".to_string(), "auto".to_string()]
        )]
    );

    n.src.start(&vm, None).await.unwrap();
    let bitmaps = n.src.backup_bitmaps(vm.id).await.unwrap();
    assert!(bitmaps[0].bitmaps.is_empty(), "{bitmaps:?}");
    n.src.block_resize(vm.id, 0, 2 << 30).await.unwrap();

    // 보내는 도중에 실패해도(받는 쪽의 메모리 크기가 달라 받지 못한다) 끝날 때 비트맵을 깨끗이
    // 저장한다.
    n.src.add_bitmaps(vm.id, "opl-b").await.unwrap();
    let task = Uuid::now_v7();
    let other = VmRuntimeSpec {
        memory_mib: 512,
        ..vm.clone()
    };
    let port = n
        .dst
        .start_incoming(&other, task, HOST, 61040..=61059, &[])
        .await
        .unwrap()
        .migration;
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    let err = n.src.migrate(vm.id, &target, &opts()).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    n.src.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(
        image_bitmaps(&disk),
        [("opl-b".to_string(), vec!["auto".to_string()])]
    );

    // 비트맵은 공유 이미지로 넘어간다.
    n.src.start(&vm, None).await.unwrap();
    let port = n
        .dst
        .start_incoming(&vm, task, HOST, 61040..=61059, &[])
        .await
        .unwrap()
        .migration;
    let target = MigrateTarget {
        host: HOST.into(),
        port,
        tls_hostname: HOST.into(),
    };
    n.src.migrate(vm.id, &target, &opts()).await.unwrap();
    n.dst.incoming_wait(vm.id).await.unwrap();
    n.dst.finish_incoming(vm.id).await.unwrap();
    assert!(n.src.stop_migrated(vm.id).await.unwrap());
    let bitmaps = n.dst.backup_bitmaps(vm.id).await.unwrap();
    let b = &bitmaps[0].bitmaps;
    assert_eq!(b.len(), 1, "{bitmaps:?}");
    assert_eq!(
        (b[0].name.as_str(), b[0].recording, b[0].inconsistent),
        ("opl-b", true, false)
    );
    n.dst.stop(vm.id, StopMode::Force).await.unwrap();
    assert_eq!(
        image_bitmaps(&disk),
        [("opl-b".to_string(), vec!["auto".to_string()])]
    );
}

/// 디스크 순서대로 (경로, 형식)인 디스크를 가진 VM. 노드마다 같은 id로 디스크만 다르게 만든다.
fn spec_with(id: Uuid, disks: &[(PathBuf, ImageFormat)]) -> VmRuntimeSpec {
    let disk = spec(PathBuf::new()).disks.remove(0);
    VmRuntimeSpec {
        id,
        disks: disks
            .iter()
            .enumerate()
            .map(|(i, (path, format))| RuntimeDisk {
                path: path.clone(),
                format: *format,
                boot_order: (i == 0).then_some(1),
                ..disk.clone()
            })
            .collect(),
        ..spec(PathBuf::new())
    }
}

/// 빈 이미지 `path`(형식 `format`, [`DISK_SIZE`])
fn create_image(format: &str, path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out = std::process::Command::new("qemu-img")
        .args(["create", "-q", "-f", format])
        .arg(path)
        .arg(DISK_SIZE)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

/// 이미지 `path`(형식 `format`)를 `qemu-io`로 읽어 무늬를 확인하거나 쓴다. 읽기는 잠금 없이(`-U`).
fn qemu_io(format: &str, command: &str, path: &Path) -> String {
    let mut cmd = std::process::Command::new("qemu-io");
    if command.starts_with("read") {
        cmd.args(["-r", "-U"]);
    }
    let out = cmd
        .args(["-f", format, "-c", command])
        .arg(path)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success() && !text.contains("Pattern verification failed"),
        "qemu-io {command} {path:?}: {text}"
    );
    text
}

/// 실행 중인 VM의 `disk`번째 디스크에 게스트가 쓴 것처럼 쓴다(HMP `qemu-io`, 게스트 OS 없음).
async fn guest_write(hv: &QemuHypervisor, id: Uuid, disk: usize, write: &str) {
    let line = format!("qemu-io -d /machine/peripheral/disk{disk}-dev/virtio-backend \"{write}\"");
    let out = hv.human_monitor_command(id, &line).await.unwrap();
    assert!(!out.contains("rror"), "{line}: {out}");
}

fn mirror_targets(port: u16, formats: &[ImageFormat], tls_hostname: &str) -> Vec<MirrorTarget> {
    formats
        .iter()
        .enumerate()
        .map(|(disk, format)| MirrorTarget {
            disk,
            host: HOST.into(),
            port,
            export: format!("disk{disk}"),
            tls_hostname: tls_hostname.into(),
            format: *format,
        })
        .collect()
}

fn mirror_opts(
    speed: Option<u64>,
) -> (MirrorOptions, tokio::sync::watch::Receiver<MirrorProgress>) {
    let (tx, rx) = tokio::sync::watch::channel(MirrorProgress::default());
    let opts = MirrorOptions {
        speed,
        timeout: Duration::from_secs(120),
        stall: Duration::from_secs(60),
        progress: Some(tx),
    };
    (opts, rx)
}

const DISK_SIZE: &str = "64M";

/// 로컬 디스크 둘(qcow2 → qcow2, qcow2 → raw)을 대상의 TLS NBD 내보내기로 미러하며 옮긴다. 복사 중과
/// ready 뒤의 게스트 쓰기도 대상에 있고, 같은 형식 디스크의 비트맵은 대상 qcow2에 저장된다.
#[tokio::test]
async fn migrates_local_disks_by_mirroring_them() {
    let n = nodes().await;
    let (a, b) = (n.tmp.path().join("a"), n.tmp.path().join("b"));
    let src_disks = [a.join("d0.qcow2"), a.join("d1.qcow2")];
    for disk in &src_disks {
        create_image("qcow2", disk);
        // 일부만 할당된 디스크(나머지는 성기게 남는다)
        qemu_io("qcow2", "write -P 0x11 0 4M", disk);
        qemu_io("qcow2", "write -P 0x22 32M 1M", disk);
    }
    let dst_disks = [b.join("d0.qcow2"), b.join("d1.raw")];
    create_image("qcow2", &dst_disks[0]);
    create_image("raw", &dst_disks[1]);
    let id = Uuid::now_v7();
    let src_vm = spec_with(
        id,
        &[
            (src_disks[0].clone(), ImageFormat::Qcow2),
            (src_disks[1].clone(), ImageFormat::Qcow2),
        ],
    );
    let formats = [ImageFormat::Qcow2, ImageFormat::Raw];
    let dst_vm = spec_with(
        id,
        &[
            (dst_disks[0].clone(), formats[0]),
            (dst_disks[1].clone(), formats[1]),
        ],
    );
    n.src.start(&src_vm, None).await.unwrap();
    n.src.add_bitmaps(id, "opl-a").await.unwrap();

    let task = Uuid::now_v7();
    let ports = n
        .dst
        .start_incoming(&dst_vm, task, HOST, 61060..=61079, &[0, 1])
        .await
        .unwrap();
    let nbd = ports.nbd.unwrap();
    assert_ne!(nbd, ports.migration);
    assert!((61060..=61079).contains(&nbd));

    // 1 MiB/s로 복사하는 동안 게스트가 쓴다.
    let targets = mirror_targets(nbd, &formats, HOST);
    let (mirror, rx) = mirror_opts(Some(1 << 20));
    let (copied, ()) = tokio::join!(n.src.mirror_disks(id, &targets, &mirror), async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        guest_write(&n.src, id, 0, "write -P 0x44 16M 1M").await;
    });
    copied.unwrap();
    let progress = *rx.borrow();
    assert!(progress.total_bytes >= 10 << 20, "{progress:?}");
    assert_eq!(progress.done_bytes, progress.total_bytes);
    // qcow2 → raw로 옮기는 disk1의 비트맵은 미러 전에 지웠다.
    let bitmaps = n.src.backup_bitmaps(id).await.unwrap();
    assert_eq!(bitmaps[0].bitmaps.len(), 1, "{bitmaps:?}");
    assert!(bitmaps[1].bitmaps.is_empty(), "{bitmaps:?}");

    // ready 뒤의 쓰기는 스위치오버에서 맞춘다.
    guest_write(&n.src, id, 0, "write -P 0x55 20M 64k").await;
    guest_write(&n.src, id, 1, "write -P 0x66 24M 64k").await;
    let target = MigrateTarget {
        host: HOST.into(),
        port: ports.migration,
        tls_hostname: HOST.into(),
    };
    let opts = MigrateOptions {
        switchover_mirrors: true,
        ..opts()
    };
    n.src.migrate(id, &target, &opts).await.unwrap();
    n.dst.incoming_wait(id).await.unwrap();
    n.dst.finish_incoming(id).await.unwrap();
    assert_eq!(status(&n.dst, id).await, Some(RuntimeStatus::Running));
    // 내보내기를 닫았다.
    assert!(std::net::TcpListener::bind((HOST, nbd)).is_ok());
    let bitmaps = n.dst.backup_bitmaps(id).await.unwrap();
    assert_eq!(bitmaps[0].bitmaps.len(), 1, "{bitmaps:?}");
    assert_eq!(bitmaps[0].bitmaps[0].name, "opl-a");
    assert!(bitmaps[1].bitmaps.is_empty(), "{bitmaps:?}");
    assert!(n.src.stop_migrated(id).await.unwrap());
    n.dst.stop(id, StopMode::Force).await.unwrap();

    for (src, (dst, format)) in src_disks.iter().zip(dst_disks.iter().zip(["qcow2", "raw"])) {
        let out = std::process::Command::new("qemu-img")
            .args(["compare", "-U", "-f", "qcow2", "-F", format])
            .arg(src)
            .arg(dst)
            .output()
            .unwrap();
        assert!(out.status.success(), "{src:?} {dst:?}: {out:?}");
    }
    for read in [
        "read -P 0x11 0 4M",
        "read -P 0x44 16M 1M",
        "read -P 0x55 20M 64k",
    ] {
        qemu_io("qcow2", read, &dst_disks[0]);
    }
    qemu_io("raw", "read -P 0x66 24M 64k", &dst_disks[1]);
    qemu_io("raw", "read -P 0x22 32M 1M", &dst_disks[1]);
    // 같은 형식 디스크의 비트맵은 대상 qcow2에 저장됐다.
    assert_eq!(
        image_bitmaps(&dst_disks[0]),
        [("opl-a".to_string(), vec!["auto".to_string()])]
    );
}

/// 미러하지 못하거나(닫힌 포트, 노드 인증서에 없는 이름) 메모리 단계가 실패해도 원본은 계속 돌고,
/// 되돌리기가 남은 미러를 치운다.
#[tokio::test]
async fn failed_disk_copy_keeps_the_source_running() {
    let n = nodes().await;
    let (a, b) = (n.tmp.path().join("a"), n.tmp.path().join("b"));
    let (src_disk, dst_disk) = (a.join("d0.qcow2"), b.join("d0.qcow2"));
    for disk in [&src_disk, &dst_disk] {
        create_image("qcow2", disk);
    }
    qemu_io("qcow2", "write -P 0x11 0 1M", &src_disk);
    let id = Uuid::now_v7();
    let src_vm = spec_with(id, &[(src_disk.clone(), ImageFormat::Qcow2)]);
    let dst_vm = spec_with(id, &[(dst_disk.clone(), ImageFormat::Qcow2)]);
    let formats = [ImageFormat::Qcow2];
    n.src.start(&src_vm, None).await.unwrap();

    let closed = {
        let l = std::net::TcpListener::bind((HOST, 0)).unwrap();
        l.local_addr().unwrap().port()
    };
    let (opts, _rx) = mirror_opts(None);
    let err = n
        .src
        .mirror_disks(id, &mirror_targets(closed, &formats, HOST), &opts)
        .await
        .unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
    assert_eq!(status(&n.src, id).await, Some(RuntimeStatus::Running));

    let task = Uuid::now_v7();
    let ports = n
        .dst
        .start_incoming(&dst_vm, task, HOST, 61080..=61099, &[0])
        .await
        .unwrap();
    let nbd = ports.nbd.unwrap();
    // 받는 노드 인증서에 없는 이름: TLS가 실패한다.
    let err = n
        .src
        .mirror_disks(id, &mirror_targets(nbd, &formats, "not-the-node"), &opts)
        .await
        .unwrap_err();
    assert!(!err.is_transient(), "{err:?}");
    assert_eq!(status(&n.src, id).await, Some(RuntimeStatus::Running));

    // 미러가 ready인데 받는 QEMU가 사라지면 메모리 단계가 실패한다. 되돌리기가 미러를 치운다.
    n.src
        .mirror_disks(id, &mirror_targets(nbd, &formats, HOST), &opts)
        .await
        .unwrap();
    n.dst.stop(id, StopMode::Force).await.unwrap();
    let target = MigrateTarget {
        host: HOST.into(),
        port: ports.migration,
        tls_hostname: HOST.into(),
    };
    let migrate = MigrateOptions {
        switchover_mirrors: true,
        ..opts_short()
    };
    let err = n.src.migrate(id, &target, &migrate).await.unwrap_err();
    assert!(matches!(err, DriverError::Migration(_)), "{err:?}");
    n.src.cancel_migration(id).await.unwrap();
    if status(&n.src, id).await != Some(RuntimeStatus::Running) {
        n.src.resume(id).await.unwrap();
    }
    assert_eq!(status(&n.src, id).await, Some(RuntimeStatus::Running));
    // 미러가 남지 않아 스냅샷이 된다.
    n.src.snapshot_save(id, "after", false).await.unwrap();
    n.src.stop(id, StopMode::Force).await.unwrap();
}

fn opts_short() -> MigrateOptions {
    MigrateOptions {
        timeout: Duration::from_secs(30),
        ..opts()
    }
}
