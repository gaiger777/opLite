//! 실제 qemu-img를 쓰는 스토리지 통합 테스트. `cargo test -p opl-driver --features storage-it`
#![cfg(feature = "storage-it")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use opl_driver::{
    Capacity, DirStorage, DriverError, SourceFormat, Storage, Storages, VolumeInfo, VolumeSource,
};
use opl_types::resource::Resource;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePool, StoragePoolSpec};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

/// `dir` 풀만 쓰므로 NFS 마운트 루트는 쓰이지 않는다.
const MNT: &str = "/nonexistent/opl-pools";

fn pool(dir: &Path) -> StoragePool {
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

fn volume_path(pool: &StoragePool, id: Uuid, format: ImageFormat) -> PathBuf {
    DirStorage::volume_path(pool, Path::new(MNT), id, format)
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
    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();

    let path = storage
        .create_volume(&pool, id, 64 * MIB, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert_eq!(path, volume_path(&pool, id, ImageFormat::Qcow2));
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

    let storage = DirStorage::new("qemu-img", MNT);
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
    let storage = DirStorage::new("qemu-img", MNT);
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
        matches!(err, DriverError::InvalidSpec(ref m) if m.contains("descriptor")),
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
    let storage = DirStorage::new("qemu-img", MNT);
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
    assert!(!volume_path(&pool, id, ImageFormat::Qcow2).exists());
}

/// `qemu-io` 명령 하나를 실행하고 성공했는지 돌려준다. `read -P`는 패턴이 다르면 실패한다.
fn qemu_io(path: &Path, cmd: &str) -> bool {
    qemu_io_fmt(path, ImageFormat::Qcow2, cmd)
}

fn qemu_io_fmt(path: &Path, format: ImageFormat, cmd: &str) -> bool {
    let format = match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    };
    std::process::Command::new("qemu-io")
        .args(["-f", format, "-c", cmd, path.to_str().unwrap()])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test]
async fn snapshot_revert_restores_contents_and_delete_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    // 경로의 쉼표는 `--image-opts` 옵션 문자열에서 `,,`로 써야 한다.
    let dir = tmp.path().join("a,b");
    std::fs::create_dir(&dir).unwrap();
    let pool = pool(&dir);
    let storage = DirStorage::new("qemu-img", MNT);
    let id = Uuid::now_v7();
    let qcow2 = ImageFormat::Qcow2;
    let path = storage
        .create_volume(&pool, id, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(storage.supports_snapshots(&pool, qcow2));
    assert!(!storage.supports_snapshots(&pool, ImageFormat::Raw));

    assert!(qemu_io(&path, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage
        .snapshot_create(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert_eq!(
        storage.snapshot_tags(&pool, id, qcow2).await.unwrap(),
        vec![tag.clone()]
    );
    // 같은 태그를 또 만들지 않는다(qemu-img는 이름이 겹치는 스냅샷을 허용한다).
    let dup = storage.snapshot_create(&pool, id, qcow2, &tag).await;
    assert!(matches!(dup, Err(DriverError::InvalidSpec(_))), "{dup:?}");

    assert!(qemu_io(&path, "write -P 0xcd 0 64k"));
    storage
        .snapshot_revert(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert!(qemu_io(&path, "read -P 0xab 0 64k"));

    let missing = Uuid::now_v7().to_string();
    let err = storage
        .snapshot_revert(&pool, id, qcow2, &missing)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");

    storage
        .snapshot_delete(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    assert!(
        storage
            .snapshot_tags(&pool, id, qcow2)
            .await
            .unwrap()
            .is_empty()
    );
    // 태그가 이미 없어도 성공한다(다시 시도해도 된다).
    storage
        .snapshot_delete(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    // 스냅샷을 지워도 현재 내용은 그대로다.
    assert!(qemu_io(&path, "read -P 0xab 0 64k"));
}

#[tokio::test]
async fn resize_grows_volume_with_snapshots() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img", MNT);

    for format in [ImageFormat::Qcow2, ImageFormat::Raw] {
        let id = Uuid::now_v7();
        storage
            .create_volume(&pool, id, 64 * MIB, format)
            .await
            .unwrap();
        if format == ImageFormat::Qcow2 {
            // 내부 스냅샷이 있어도 크기를 늘릴 수 있다.
            storage
                .snapshot_create(&pool, id, format, "s")
                .await
                .unwrap();
        }
        storage
            .resize_volume(&pool, id, format, 128 * MIB)
            .await
            .unwrap();
        let info = storage.volume_info(&pool, id, format).await.unwrap();
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");

        // 줄이기는 하지 않는다.
        let err = storage
            .resize_volume(&pool, id, format, 32 * MIB)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        let info = storage.volume_info(&pool, id, format).await.unwrap();
        assert_eq!(info.virtual_bytes, 128 * MIB, "{format:?}");
    }
}

#[tokio::test]
async fn copy_volume_copies_snapshot_or_current_contents() {
    let tmp = tempfile::tempdir().unwrap();
    // 다른 풀로 복사한다. 경로의 쉼표는 옵션 문자열에서 `,,`로 써야 한다.
    let (src_dir, dst_dir) = (tmp.path().join("src"), tmp.path().join("d,st"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&dst_dir).unwrap();
    let (src_pool, dst_pool) = (pool(&src_dir), pool(&dst_dir));
    let storage = DirStorage::new("qemu-img", MNT);
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storage
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 64k"));
    let tag = Uuid::now_v7().to_string();
    storage
        .snapshot_create(&src_pool, src, qcow2, &tag)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xcd 0 64k"));

    // 스냅샷 때의 내용. -U(실행 중인 VM의 스냅샷에서 복사할 때)도 받아들인다.
    for force_share in [false, true] {
        let dst = Uuid::now_v7();
        storage
            .copy_volume(
                &src_pool,
                src,
                qcow2,
                Some(&tag),
                force_share,
                &dst_pool,
                dst,
                qcow2,
            )
            .await
            .unwrap();
        let dst_path = volume_path(&dst_pool, dst, qcow2);
        assert!(qemu_io(&dst_path, "read -P 0xab 0 64k"), "{force_share}");
        let info = storage.volume_info(&dst_pool, dst, qcow2).await.unwrap();
        assert_eq!(info.virtual_bytes, 64 * MIB);
        // 사본에는 스냅샷이 따라오지 않는다.
        assert!(
            storage
                .snapshot_tags(&dst_pool, dst, qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    // 태그가 없으면 지금 내용
    let now = Uuid::now_v7();
    storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(
        &volume_path(&dst_pool, now, qcow2),
        "read -P 0xcd 0 64k"
    ));
    // 대상이 있으면 덮어쓰지 않는다.
    let err = storage
        .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, now, qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");

    // 없는 태그는 실패하고 파일을 남기지 않는다.
    let missing = Uuid::now_v7();
    let err = storage
        .copy_volume(
            &src_pool,
            src,
            qcow2,
            Some("gone"),
            false,
            &dst_pool,
            missing,
            qcow2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert_eq!(std::fs::read_dir(&dst_dir).unwrap().count(), 3);
    assert!(!volume_path(&dst_pool, missing, qcow2).exists());

    // raw도 같은 형식으로 복사한다.
    let raw = ImageFormat::Raw;
    let src_raw = Uuid::now_v7();
    let src_raw_path = storage
        .create_volume(&src_pool, src_raw, 64 * MIB, raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(&src_raw_path, raw, "write -P 0x5a 0 64k"));
    let dst_raw = Uuid::now_v7();
    storage
        .copy_volume(
            &src_pool, src_raw, raw, None, false, &dst_pool, dst_raw, raw,
        )
        .await
        .unwrap();
    let dst_raw_path = volume_path(&dst_pool, dst_raw, raw);
    assert!(qemu_io_fmt(&dst_raw_path, raw, "read -P 0x5a 0 64k"));
    let info = storage.volume_info(&dst_pool, dst_raw, raw).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);
}

/// 다른 백엔드 대신 쓰는 저장소: 어떤 풀이 와도 디렉터리 풀 `dir`의 파일로 다룬다.
struct InDir {
    storage: DirStorage,
    dir: StoragePool,
}

#[async_trait]
impl Storage for InDir {
    async fn create_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.storage
            .create_volume(&self.dir, id, size_bytes, format)
            .await
    }
    async fn import_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.storage
            .import_volume(&self.dir, id, src, src_format, format)
            .await
    }
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        self.storage.check_import_source(src, src_format).await
    }
    async fn delete_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.storage.delete_volume(&self.dir, id, format).await
    }
    async fn activate_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.storage.activate_volume(&self.dir, id, format).await
    }
    async fn volume_info(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        self.storage.volume_info(&self.dir, id, format).await
    }
    async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
        self.storage.capacity(&self.dir).await
    }
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        self.storage.supports_snapshots(&self.dir, format)
    }
    async fn snapshot_create(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.storage
            .snapshot_create(&self.dir, id, format, tag)
            .await
    }
    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.storage
            .snapshot_revert(&self.dir, id, format, tag)
            .await
    }
    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.storage
            .snapshot_delete(&self.dir, id, format, tag)
            .await
    }
    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        self.storage.snapshot_tags(&self.dir, id, format).await
    }
    async fn resize_volume(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.storage
            .resize_volume(&self.dir, id, format, size_bytes)
            .await
    }
    async fn volume_source(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        self.storage.volume_source(&self.dir, id, format, tag).await
    }
    async fn release_source(
        &self,
        _: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        self.storage
            .release_source(&self.dir, id, format, tag)
            .await
    }
    async fn copy_volume(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
        _: bool,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<(), DriverError> {
        unreachable!("copies between backends go through Storages")
    }
}

/// 백엔드가 다른 풀 사이 복사는 대상에 만든 볼륨에 `qemu-img convert -n`으로 써 넣는다.
/// 같은 백엔드 사이 복사는 그 저장소가 그대로 한다.
#[tokio::test]
async fn storages_copy_between_backends() {
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, other_dir) = (tmp.path().join("src"), tmp.path().join("other"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&other_dir).unwrap();
    let src_pool = pool(&src_dir);
    let other = InDir {
        storage: DirStorage::new("qemu-img", MNT),
        dir: pool(&other_dir),
    };
    let other_pool = Resource::new(
        "lvm",
        StoragePoolSpec {
            backend: PoolBackend::LvmThin {
                vg: "vg0".into(),
                thinpool: "tp".into(),
            },
            nodes: vec![],
            shared: false,
        },
    );
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)))
        .with_lvm_thin(Arc::new(other));
    let other_path = |id, format| volume_path(&pool(&other_dir), id, format);
    let qcow2 = ImageFormat::Qcow2;

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 64k"));
    storages
        .snapshot_create(&src_pool, src, qcow2, "s1")
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xcd 0 64k"));

    // 스냅샷 때의 내용을 다른 백엔드로
    let dst = Uuid::now_v7();
    storages
        .copy_volume(
            &src_pool,
            src,
            qcow2,
            Some("s1"),
            true,
            &other_pool,
            dst,
            qcow2,
        )
        .await
        .unwrap();
    assert!(qemu_io(&other_path(dst, qcow2), "read -P 0xab 0 64k"));
    let info = storages.volume_info(&other_pool, dst, qcow2).await.unwrap();
    assert_eq!(info.virtual_bytes, 64 * MIB);

    // 다시 돌아오는 방향, 지금 내용
    let back = Uuid::now_v7();
    storages
        .copy_volume(&other_pool, dst, qcow2, None, false, &src_pool, back, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(
        &volume_path(&src_pool, back, qcow2),
        "read -P 0xab 0 64k"
    ));

    // 없는 태그는 실패하고 만든 볼륨을 지운다.
    let missing = Uuid::now_v7();
    let err = storages
        .copy_volume(
            &src_pool,
            src,
            qcow2,
            Some("gone"),
            false,
            &other_pool,
            missing,
            qcow2,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(!other_path(missing, qcow2).exists());

    // 같은 백엔드 사이(디렉터리 풀끼리)는 파일 저장소의 복사다.
    let same = Uuid::now_v7();
    storages
        .copy_volume(&src_pool, src, qcow2, None, false, &src_pool, same, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(
        &volume_path(&src_pool, same, qcow2),
        "read -P 0xcd 0 64k"
    ));
}
