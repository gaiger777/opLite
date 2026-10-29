//! 실제 ZFS 풀의 저장소 통합 테스트. 관리자가 만든 데이터셋(`OPL_TEST_ZFS=POOL/DATASET`)에 볼륨
//! zvol을 만들고 가져오고, ZFS 스냅샷을 찍어 되돌리고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_ZFS=opltestz/opl cargo test -p opl-driver --features zfs-it --test zfs_storage`
//! Linux에서 root, zfsutils-linux, qemu-img, qemu-io가 필요하다.
#![cfg(all(feature = "zfs-it", target_os = "linux"))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use opl_driver::{DirStorage, DriverError, SourceFormat, Storage, Storages, SystemZfs, ZfsStorage};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// `OPL_TEST_ZFS`의 데이터셋
fn dataset() -> String {
    std::env::var("OPL_TEST_ZFS").expect("OPL_TEST_ZFS must name POOL/DATASET")
}

fn zfs_pool() -> StoragePool {
    Resource::new(
        "zfs",
        StoragePoolSpec {
            backend: PoolBackend::Zfs { dataset: dataset() },
            nodes: vec![],
            shared: false,
        },
    )
}

fn dir_pool(dir: &Path) -> StoragePool {
    Resource::new(
        "local",
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: dir.to_path_buf(),
            },
            nodes: vec![],
            shared: false,
        },
    )
}

fn storage() -> ZfsStorage {
    ZfsStorage::new("qemu-img", Arc::new(SystemZfs))
}

/// 데이터셋 아래 `name`(zvol, 또는 `zvol@tag` 스냅샷)의 장치 노드
fn device(name: &str) -> PathBuf {
    Path::new("/dev/zvol").join(dataset()).join(name)
}

fn volume_device(id: Uuid) -> PathBuf {
    device(&format!("opl-{id}"))
}

/// 데이터셋 아래 zvol과 스냅샷 이름(데이터셋 부분을 뺀 것) 중 `prefix`로 시작하는 것
fn zfs_names(prefix: &str) -> Vec<String> {
    let dataset = dataset();
    let out = Command::new("zfs")
        .args(["list", "-H", "-o", "name", "-t", "volume,snapshot", "-r"])
        .arg(&dataset)
        .output()
        .expect("run zfs list");
    assert!(out.status.success(), "{out:?}");
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().strip_prefix(&format!("{dataset}/")))
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

/// 지운 zvol의 장치 노드는 udev가 조금 뒤에 없앤다.
fn wait_gone(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < deadline, "{} remains", path.display());
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn qemu_img(args: &[&str]) {
    let status = Command::new("qemu-img").args(args).status().unwrap();
    assert!(status.success(), "qemu-img {args:?} failed");
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, format: ImageFormat, cmd: &str) -> bool {
    let format = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    let mut qemu_io = Command::new("qemu-io");
    // 읽기는 읽기 전용으로 연다: ZFS 스냅샷 장치는 쓰기로 열 수 없다.
    if cmd.starts_with("read") {
        qemu_io.arg("-r");
    }
    qemu_io
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_inspect_resize_and_delete_volume() {
    let pool = zfs_pool();
    let storage = storage();
    let id = Uuid::now_v7();

    // 크기는 1 MiB 배수로 올린다.
    let path = storage
        .create_volume(&pool, id, 64 * MIB - 1, RAW)
        .await
        .unwrap();
    assert_eq!(path, volume_device(id));
    assert!(path.exists(), "{}", path.display());
    assert_eq!(zfs_names(&format!("opl-{id}")), vec![format!("opl-{id}")]);
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert!(info.allocated_bytes < info.virtual_bytes, "{info:?}");
    assert!(qemu_io(&path, RAW, "write -P 0xab 0 1M"));
    let (location, _) = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap();
    assert_eq!(location, path);
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    let capacity = storage.capacity(&pool).await.unwrap();
    assert!(capacity.total_bytes > 0, "{capacity:?}");
    assert!(
        capacity.available_bytes < capacity.total_bytes,
        "{capacity:?}"
    );

    let again = storage.create_volume(&pool, id, 64 * MIB, RAW).await;
    assert!(
        matches!(again, Err(DriverError::VolumeExists(_))),
        "{again:?}"
    );
    let qcow2 = storage
        .create_volume(&pool, Uuid::now_v7(), 64 * MIB, ImageFormat::Qcow2)
        .await;
    assert!(
        matches!(qcow2, Err(DriverError::InvalidSpec(_))),
        "{qcow2:?}"
    );

    // 늘리면 내용은 그대로다. 이미 그 크기 이상이면 아무것도 하지 않는다(줄이지 않는다).
    storage
        .resize_volume(&pool, id, RAW, 128 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 1M"));
    storage
        .resize_volume(&pool, id, RAW, 32 * MIB)
        .await
        .unwrap();
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 128 * MIB);

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(zfs_names(&format!("opl-{id}")).is_empty());
    wait_gone(&path);
    let gone = storage.volume_info(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let gone = storage.delete_volume(&pool, id, RAW).await;
    assert!(
        matches!(gone, Err(DriverError::VolumeNotFound(_))),
        "{gone:?}"
    );
    let err = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap_err();
    assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = zfs_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk
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
    storage
        .check_import_source(&vmdk, SourceFormat::Vmdk)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    let path = storage
        .import_volume(&pool, id, &vmdk, SourceFormat::Vmdk, RAW)
        .await
        .unwrap();
    assert_eq!(path, volume_device(id));
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, MIB, "{info:?}");
    let mut head = vec![0u8; MIB as usize];
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert!(head == pattern, "imported contents differ");
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 zvol을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .import_volume(
            &pool,
            missing,
            &tmp.path().join("missing.vmdk"),
            SourceFormat::Vmdk,
            RAW,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(zfs_names(&format!("opl-{missing}")).is_empty());
}

#[tokio::test]
async fn snapshots_revert_to_the_latest_and_go_with_the_volume() {
    let pool = zfs_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    let path = storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, RAW));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Qcow2));
    let zvol = format!("opl-{id}");

    assert!(qemu_io(&path, RAW, "write -P 0xab 0 64k"));
    let s1 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s1).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone()]
    );
    assert_eq!(zfs_names(&zvol), vec![zvol.clone(), format!("{zvol}@{s1}")]);
    let dup = storage.snapshot_create(&pool, id, RAW, &s1).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    // 가장 최근 스냅샷으로 되돌리면 그때 내용이 같은 장치에 돌아오고 스냅샷은 남는다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    // 더 최근 스냅샷이 있으면 되돌리지 않고 아무것도 바꾸지 않는다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    let s2 = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &s2).await.unwrap();
    assert!(qemu_io(&path, RAW, "write -P 0x5a 0 64k"));
    let err = storage
        .snapshot_revert(&pool, id, RAW, &s1)
        .await
        .unwrap_err();
    match err {
        DriverError::InvalidSpec(msg) => assert!(msg.contains("latest snapshot"), "{msg}"),
        e => panic!("{e:?}"),
    }
    assert!(qemu_io(&path, RAW, "read -P 0x5a 0 64k"));
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![s1.clone(), s2.clone()]
    );
    // 최근 것을 지우면 그 앞 스냅샷으로 되돌릴 수 있다.
    storage.snapshot_delete(&pool, id, RAW, &s2).await.unwrap();
    storage.snapshot_revert(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, RAW, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");

    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(
        storage
            .snapshot_tags(&pool, id, RAW)
            .await
            .unwrap()
            .is_empty()
    );
    // 이미 없어도 성공한다.
    storage.snapshot_delete(&pool, id, RAW, &s1).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    // 여러 볼륨을 한 번에 찍는다. 볼륨을 지우면 남은 스냅샷도 함께 지운다.
    let other = Uuid::now_v7();
    storage
        .create_volume(&pool, other, 64 * MIB, RAW)
        .await
        .unwrap();
    storage
        .snapshot_many(&pool, &[id, other], RAW, "both")
        .await
        .unwrap();
    for v in [id, other] {
        assert_eq!(
            storage.snapshot_tags(&pool, v, RAW).await.unwrap(),
            vec!["both".to_string()]
        );
    }
    // 한 볼륨에라도 그 태그가 있으면 어느 볼륨에도 찍지 않는다.
    let third = Uuid::now_v7();
    storage
        .create_volume(&pool, third, 64 * MIB, RAW)
        .await
        .unwrap();
    let dup = storage
        .snapshot_many(&pool, &[third, other], RAW, "both")
        .await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");
    assert!(
        storage
            .snapshot_tags(&pool, third, RAW)
            .await
            .unwrap()
            .is_empty()
    );
    for v in [id, other, third] {
        storage.delete_volume(&pool, v, RAW).await.unwrap();
        assert!(zfs_names(&format!("opl-{v}")).is_empty());
    }
}

#[tokio::test]
async fn copies_snapshots_within_and_across_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = zfs_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_zfs(Arc::new(storage()));
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&pool, src, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, RAW, "write -P 0xab 0 64k"));
    storages
        .snapshot_create(&pool, src, RAW, "s1")
        .await
        .unwrap();
    assert!(qemu_io(&src_path, RAW, "write -P 0xcd 0 64k"));
    // `snapdev=visible`이라 스냅샷도 장치로 읽는다.
    assert!(qemu_io(
        &device(&format!("opl-{src}@s1")),
        RAW,
        "read -P 0xab 0 64k"
    ));

    // 같은 풀 안: 스냅샷 때의 내용을 새 zvol로
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(copy), RAW, "read -P 0xab 0 64k"));
    let info = storages.volume_info(&pool, copy, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    // 사본에는 스냅샷이 따라오지 않는다.
    assert!(
        storages
            .snapshot_tags(&pool, copy, RAW)
            .await
            .unwrap()
            .is_empty()
    );

    // 디렉터리 풀로: 스냅샷 때의 내용을 qcow2로
    let out = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &local, out, qcow2)
        .await
        .unwrap();
    let out_path = DirStorage::volume_path(&local, Path::new(MNT), out, qcow2);
    assert!(qemu_io(&out_path, qcow2, "read -P 0xab 0 64k"));

    // 디렉터리 풀에서 돌아오기: qcow2 → raw zvol, 지금 내용
    assert!(qemu_io(&out_path, qcow2, "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(back), RAW, "read -P 0x5a 0 64k"));

    // 없는 스냅샷은 실패하고 대상 zvol을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("gone"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(zfs_names(&format!("opl-{missing}")).is_empty());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(zfs_names(&format!("opl-{id}")).is_empty());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}
