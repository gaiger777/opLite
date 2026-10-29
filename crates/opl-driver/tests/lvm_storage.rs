//! 실제 LVM-thin 풀의 저장소 통합 테스트. 관리자가 만든 VG와 thin pool(`OPL_TEST_LVM=VG/THINPOOL`)에
//! 볼륨 LV를 만들고 가져오고, thin 스냅샷을 만들어 되돌리고, 복사하고 지운다.
//! `sudo -E env "PATH=$PATH" OPL_TEST_LVM=opltest/tp cargo test -p opl-driver --features lvm-it --test lvm_storage`
//! Linux에서 root, lvm2(thin-provisioning-tools), qemu-img, qemu-io가 필요하다.
#![cfg(all(feature = "lvm-it", target_os = "linux"))]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use opl_driver::{
    DirStorage, DriverError, LvmThinStorage, SourceFormat, Storage, Storages, SystemLvm,
};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

const RAW: ImageFormat = ImageFormat::Raw;

/// `OPL_TEST_LVM`의 VG와 thin pool
fn vg_and_thinpool() -> (String, String) {
    let target = std::env::var("OPL_TEST_LVM").expect("OPL_TEST_LVM must name VG/THINPOOL");
    let (vg, thinpool) = target
        .split_once('/')
        .expect("OPL_TEST_LVM must look like VG/THINPOOL");
    (vg.to_string(), thinpool.to_string())
}

fn lvm_pool() -> StoragePool {
    let (vg, thinpool) = vg_and_thinpool();
    Resource::new(
        "lvm",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin { vg, thinpool },
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

fn storage() -> LvmThinStorage {
    LvmThinStorage::new("qemu-img", Arc::new(SystemLvm))
}

/// LV `name`의 장치 노드
fn device(name: &str) -> PathBuf {
    Path::new("/dev").join(vg_and_thinpool().0).join(name)
}

fn volume_device(id: Uuid) -> PathBuf {
    device(&format!("opl-{id}"))
}

/// VG에 있는 LV 이름 중 `prefix`로 시작하는 것
fn lvs(prefix: &str) -> Vec<String> {
    let out = Command::new("lvs")
        .args(["--noheadings", "-o", "lv_name"])
        .arg(vg_and_thinpool().0)
        .output()
        .expect("run lvs");
    assert!(out.status.success(), "{out:?}");
    let mut names: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| l.starts_with(prefix))
        .collect();
    names.sort();
    names
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
    Command::new("qemu-io")
        .args(["-f", format, "-c", cmd])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("run qemu-io")
        .success()
}

#[tokio::test]
async fn create_inspect_resize_and_delete_volume() {
    let pool = lvm_pool();
    let storage = storage();
    let id = Uuid::now_v7();

    let path = storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert_eq!(path, volume_device(id));
    assert!(path.exists(), "{}", path.display());
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
    assert_eq!(info.allocated_bytes, 0, "a new thin lv holds no data");
    assert!(qemu_io(&path, RAW, "write -P 0xab 0 1M"));
    let info = storage.volume_info(&pool, id, RAW).await.unwrap();
    assert!(info.allocated_bytes > 0, "{info:?}");

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

    // 비활성 LV는 활성화하면 장치가 다시 생긴다.
    let (vg, _) = vg_and_thinpool();
    let lv = format!("{vg}/opl-{id}");
    let status = Command::new("lvchange")
        .args(["-an", &lv])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!path.exists());
    let err = opl_driver::disk_location(&pool, Path::new(MNT), id, RAW).unwrap_err();
    assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
    storage.activate_volume(&pool, id, RAW).await.unwrap();
    assert!(path.exists());
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 1M"));
    // 이미 활성이어도 된다.
    storage.activate_volume(&pool, id, RAW).await.unwrap();

    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(lvs(&format!("opl-{id}")).is_empty());
    assert!(!path.exists());
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
}

#[tokio::test]
async fn imports_vmdk_and_cleans_up_failed_import() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = lvm_pool();
    let storage = storage();

    // 알려진 패턴의 raw → vmdk. 가상 크기는 LV의 extent 크기로 올라간다.
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
    assert!(info.virtual_bytes >= MIB, "{info:?}");
    let mut head = vec![0u8; MIB as usize];
    std::fs::File::open(&path)
        .unwrap()
        .read_exact(&mut head)
        .unwrap();
    assert!(head == pattern, "imported contents differ");
    storage.delete_volume(&pool, id, RAW).await.unwrap();

    // 원본을 읽지 못하면 LV를 남기지 않는다.
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
    assert!(lvs(&format!("opl-{missing}")).is_empty());
}

#[tokio::test]
async fn snapshot_revert_and_delete_with_the_volume() {
    let pool = lvm_pool();
    let storage = storage();
    let id = Uuid::now_v7();
    let path = storage
        .create_volume(&pool, id, 64 * MIB, RAW)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, RAW));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Qcow2));

    assert!(qemu_io(&path, RAW, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage.snapshot_create(&pool, id, RAW, &tag).await.unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, RAW).await.unwrap(),
        vec![tag.clone()]
    );
    let snapshot_lv = format!("opl-{id}-s-{tag}");
    assert_eq!(
        lvs(&format!("opl-{id}")),
        vec![format!("opl-{id}"), snapshot_lv.clone()]
    );
    // thin 스냅샷은 활성화 건너뛰기 표시가 붙어 장치가 없다.
    assert!(!device(&snapshot_lv).exists());
    let dup = storage.snapshot_create(&pool, id, RAW, &tag).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    // 되돌리면 그때 내용이 같은 장치 경로에 돌아오고 옮겨 둔 옛 LV는 남지 않는다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &tag).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));
    assert_eq!(
        lvs(&format!("opl-{id}")),
        vec![format!("opl-{id}"), snapshot_lv.clone()]
    );
    // 스냅샷은 그대로라 다시 되돌릴 수 있다.
    assert!(qemu_io(&path, RAW, "write -P 0xcd 0 64k"));
    storage.snapshot_revert(&pool, id, RAW, &tag).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, RAW, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    storage.snapshot_delete(&pool, id, RAW, &tag).await.unwrap();
    assert!(
        storage
            .snapshot_tags(&pool, id, RAW)
            .await
            .unwrap()
            .is_empty()
    );
    // 이미 없어도 성공한다.
    storage.snapshot_delete(&pool, id, RAW, &tag).await.unwrap();
    assert!(qemu_io(&path, RAW, "read -P 0xab 0 64k"));

    // 볼륨을 지우면 남은 스냅샷 LV도 함께 지운다.
    storage.snapshot_create(&pool, id, RAW, "s1").await.unwrap();
    storage.delete_volume(&pool, id, RAW).await.unwrap();
    assert!(lvs(&format!("opl-{id}")).is_empty());
}

#[tokio::test]
async fn copies_snapshots_within_and_across_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = lvm_pool();
    let local = dir_pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_lvm_thin(Arc::new(storage()));
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
    let snapshot_device = device(&format!("opl-{src}-s-s1"));

    // 같은 풀 안: 스냅샷 때의 내용을 새 LV로. 읽으려고 활성화한 스냅샷 LV는 다시 내린다.
    let copy = Uuid::now_v7();
    storages
        .copy_volume(&pool, src, RAW, Some("s1"), true, &pool, copy, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(copy), RAW, "read -P 0xab 0 64k"));
    assert!(!snapshot_device.exists());
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
    assert!(!snapshot_device.exists());

    // 디렉터리 풀에서 돌아오기: qcow2 → raw LV, 지금 내용
    assert!(qemu_io(&out_path, qcow2, "write -P 0x5a 0 64k"));
    let back = Uuid::now_v7();
    storages
        .copy_volume(&local, out, qcow2, None, false, &pool, back, RAW)
        .await
        .unwrap();
    assert!(qemu_io(&volume_device(back), RAW, "read -P 0x5a 0 64k"));

    // 없는 스냅샷은 실패하고 대상 LV를 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(&pool, src, RAW, Some("gone"), false, &pool, missing, RAW)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    assert!(lvs(&format!("opl-{missing}")).is_empty());

    for id in [src, copy, back] {
        storages.delete_volume(&pool, id, RAW).await.unwrap();
        assert!(lvs(&format!("opl-{id}")).is_empty());
    }
    storages.delete_volume(&local, out, qcow2).await.unwrap();
}
