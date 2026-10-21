//! 실제 qemu-img를 쓰는 스토리지 통합 테스트. `cargo test -p opl-driver --features storage-it`
#![cfg(feature = "storage-it")]

use std::path::Path;

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage};
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn pool(dir: &Path) -> StoragePoolSpec {
    StoragePoolSpec {
        backend: PoolBackend::Dir {
            path: dir.to_path_buf(),
        },
        nodes: vec![],
        shared: false,
    }
}

fn qemu_img(args: &[&str]) {
    let status = std::process::Command::new("qemu-img")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

#[tokio::test]
async fn create_inspect_and_delete_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();

    let path = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(path, DirStorage::volume_path(&pool, id, ImageFormat::Qcow2));
    let info = storage
        .volume_info(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(
        info.allocated_bytes < 64 * MIB,
        "qcow2 should be thin: {info:?}"
    );

    let again = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(matches!(again, Err(DriverError::VolumeExists(_))));

    storage
        .delete_volume(&pool, id, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert!(!path.exists());
    let gone = storage.volume_info(&pool, id, ImageFormat::Qcow2).await;
    assert!(matches!(gone, Err(DriverError::VolumeNotFound(_))));
}

#[tokio::test]
async fn imports_vmdk_preserving_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let pool_dir = tmp.path().join("pool");
    std::fs::create_dir(&pool_dir).unwrap();
    let pool = pool(&pool_dir);

    // VMware에서 가져온 디스크를 흉내 낸다: 알려진 패턴의 raw → vmdk
    let pattern: Vec<u8> = (0..MIB as usize).map(|i| (i % 251) as u8).collect();
    let raw = tmp.path().join("src.raw");
    std::fs::write(&raw, &pattern).unwrap();
    let vmdk = tmp.path().join("src.vmdk");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw.to_str().unwrap(),
        vmdk.to_str().unwrap(),
    ]);

    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, ImageFormat::Qcow2)
        .await
        .unwrap();

    let back = tmp.path().join("back.raw");
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "qcow2",
        "-O",
        "raw",
        path.to_str().unwrap(),
        back.to_str().unwrap(),
    ]);
    assert_eq!(std::fs::read(&back).unwrap(), pattern);
}

#[tokio::test]
async fn import_source_must_be_a_single_file() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = DirStorage::new("qemu-img");
    let raw = tmp.path().join("disk.raw");
    std::fs::write(&raw, vec![0u8; MIB as usize]).unwrap();
    let vmdk = tmp.path().join("disk.vmdk");
    let raw_s = raw.to_str().unwrap();
    qemu_img(&[
        "convert",
        "-q",
        "-f",
        "raw",
        "-O",
        "vmdk",
        raw_s,
        vmdk.to_str().unwrap(),
    ]);
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    storage
        .check_import_source(&raw, SourceFormat::Raw)
        .await
        .unwrap();

    // monolithicFlat 디스크립터는 호스트의 임의 파일을 extent로 가리킬 수 있다.
    let desc = tmp.path().join("desc.vmdk");
    std::fs::write(
        &desc,
        format!(
            "# Disk DescriptorFile\nversion=1\nCID=fffffffe\nparentCID=ffffffff\n\
             createType=\"monolithicFlat\"\n\nRW 2048 FLAT \"{raw_s}\" 0\n"
        ),
    )
    .unwrap();
    let err = storage
        .check_import_source(&desc, SourceFormat::Vmdk)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DriverError::InvalidSpec(ref m) if m.contains(raw_s)),
        "{err:?}"
    );

    // backing file도 다른 파일이다.
    let overlay = tmp.path().join("overlay.qcow2");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-b",
        raw_s,
        "-F",
        "raw",
        overlay.to_str().unwrap(),
    ]);
    let err = storage
        .check_import_source(&overlay, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    // qcow2 외부 데이터 파일도 마찬가지다.
    let external = tmp.path().join("external.qcow2");
    let data_file = format!("data_file={raw_s},data_file_raw=on");
    qemu_img(&[
        "create",
        "-q",
        "-f",
        "qcow2",
        "-o",
        &data_file,
        external.to_str().unwrap(),
        "1M",
    ]);
    let err = storage
        .check_import_source(&external, SourceFormat::Qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
}

#[tokio::test]
async fn failed_import_leaves_no_partial_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img");
    let id = Uuid::now_v7();

    let err = storage
        .import_volume(
            &pool,
            id,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            ImageFormat::Qcow2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(!DirStorage::volume_path(&pool, id, ImageFormat::Qcow2).exists());
}
