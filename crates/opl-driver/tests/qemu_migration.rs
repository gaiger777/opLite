//! 실제 QEMU 둘로 한 호스트에서 TLS 라이브 마이그레이션을 하는 드라이버 통합 테스트.
//! `cargo test -p opl-driver --features qemu-it --test qemu_migration`
//! 두 하이퍼바이저는 실행·상태 디렉터리가 다른 두 노드를 흉내 내고, 디스크는 같은 파일(공유 풀)이다.
//! 인증서는 클러스터 PKI(`opl-peer`의 `pki`)와 같은 모양으로 rcgen이 만든다.
#![cfg(feature = "qemu-it")]

use std::path::{Path, PathBuf};
use std::time::Duration;

use opl_driver::{
    DriverError, Hypervisor, MigrateOptions, MigrateTarget, QemuConfig, QemuHypervisor,
    RuntimeStatus, StopMode, migration_tls_dir, write_migration_tls,
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
        .push(DnType::CommonName, "opLite cluster it CA");
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
    }
}

#[tokio::test]
async fn migrates_a_running_vm_over_tls() {
    let n = nodes().await;
    let vm = spec(create_disk(n.tmp.path()));
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

    // 같은 QEMU다.
    let version = n.src.running_qemu_version(vm.id).await.unwrap();
    assert_eq!(version, n.dst.qemu_version().await.unwrap());

    let port = n
        .dst
        .start_incoming(&vm, HOST, 61000..=61019)
        .await
        .unwrap();
    assert!((61000..=61019).contains(&port));
    assert_eq!(
        status(&n.dst, vm.id).await,
        Some(RuntimeStatus::Other("inmigrate".into()))
    );
    assert_eq!(n.dst.list_incoming().await.unwrap(), vec![vm.id]);
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
}

/// 확정 전에 실패하면 원본은 그대로 돈다. 받는 QEMU는 버린다.
#[tokio::test]
async fn failed_migration_keeps_the_source_running() {
    let n = nodes().await;
    let vm = spec(create_disk(n.tmp.path()));
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
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
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
        .start_incoming(&vm, HOST, 61020..=61039)
        .await
        .unwrap();
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
