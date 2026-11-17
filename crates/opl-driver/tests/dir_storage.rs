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
            backups: false,
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
            backups: false,
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

/// `qemu-img info`의 JSON
fn image_info(path: &Path) -> serde_json::Value {
    let out = std::process::Command::new("qemu-img")
        .args(["info", "--output=json", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 백업 내보내기: qcow2 볼륨의 스냅샷(실행 중인 VM처럼 `-U`)과 raw 볼륨의 지금 내용을 압축 qcow2
/// 파일로 쓴다. 내보낸 파일은 스냅샷 없는 qcow2이고 원본의 크기와 내용을 갖는다.
#[tokio::test]
async fn export_volume_writes_compressed_qcow2_of_snapshot_or_current_contents() {
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, out) = (tmp.path().join("src"), tmp.path().join("out"));
    std::fs::create_dir(&src_dir).unwrap();
    std::fs::create_dir(&out).unwrap();
    let src_pool = pool(&src_dir);
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)));
    let (qcow2, raw) = (ImageFormat::Qcow2, ImageFormat::Raw);

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 1M"));
    let tag = Uuid::now_v7().simple().to_string();
    storages
        .snapshot_create(&src_pool, src, qcow2, &tag)
        .await
        .unwrap();
    assert_eq!(
        storages.snapshot_tags(&src_pool, src, qcow2).await.unwrap(),
        vec![tag.clone()]
    );
    assert!(qemu_io(&src_path, "write -P 0xcd 0 1M"));

    let source = storages
        .volume_source(&src_pool, src, qcow2, Some(&tag))
        .await
        .unwrap();
    let dest = out.join("disk0.qcow2");
    storages.export_volume(&source, true, &dest).await.unwrap();
    storages
        .release_source(&src_pool, src, qcow2, Some(&tag))
        .await
        .unwrap();
    assert!(qemu_io(&dest, "read -P 0xab 0 1M"));
    let info = image_info(&dest);
    assert_eq!(info["format"], "qcow2");
    assert_eq!(info["virtual-size"], 64 * MIB);
    assert!(info.get("snapshots").is_none(), "{info}");
    // 압축했으므로 같은 바이트로 채운 1 MiB보다 작다.
    assert!(std::fs::metadata(&dest).unwrap().len() < MIB);
    // 원본의 태그는 그대로다.
    assert_eq!(
        storages.snapshot_tags(&src_pool, src, qcow2).await.unwrap(),
        vec![tag.clone()]
    );

    let src_raw = Uuid::now_v7();
    let raw_path = storages
        .create_volume(&src_pool, src_raw, 32 * MIB, raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(&raw_path, raw, "write -P 0x5a 0 64k"));
    let source = storages
        .volume_source(&src_pool, src_raw, raw, None)
        .await
        .unwrap();
    let dest1 = out.join("disk1.qcow2");
    storages
        .export_volume(&source, false, &dest1)
        .await
        .unwrap();
    assert!(qemu_io(&dest1, "read -P 0x5a 0 64k"));
    assert_eq!(image_info(&dest1)["virtual-size"], 32 * MIB);

    // 없는 태그면 실패하고 `.part`를 남기지 않는다.
    let gone = storages
        .volume_source(&src_pool, src, qcow2, Some("gone"))
        .await
        .unwrap();
    let err = storages
        .export_volume(&gone, true, &out.join("disk2.qcow2"))
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    let mut files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    files.sort();
    assert_eq!(files, ["disk0.qcow2", "disk1.qcow2"]);
}

/// qcow2 이미지의 dirty bitmap (이름, 플래그)
fn bitmaps(path: &Path) -> Vec<(String, Vec<String>)> {
    let info = image_info(path);
    info["format-specific"]["data"]["bitmaps"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|b| {
            let flags = b["flags"].as_array().unwrap().iter();
            let flags = flags.map(|f| f.as_str().unwrap().to_string()).collect();
            (b["name"].as_str().unwrap().to_string(), flags)
        })
        .collect()
}

/// 꺼진 qcow2 볼륨의 증분 백업 비트맵: 백업 비트맵을 하나만 남기고(복제 비트맵과 다른 비트맵은 그대로), 지울
/// 수 있다(지우기는 `opl-` 비트맵을 모두).
/// raw 볼륨은 할 일이 없다.
#[tokio::test]
async fn backup_bitmaps_of_stopped_volumes() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)));
    let (qcow2, raw) = (ImageFormat::Qcow2, ImageFormat::Raw);
    let id = Uuid::now_v7();
    let path = storages
        .create_volume(&pool, id, 64 * MIB, qcow2)
        .await
        .unwrap();
    qemu_img(&["bitmap", "--add", path.to_str().unwrap(), "user"]);
    // 복제 비트맵은 백업 비트맵이 아니다(꺼진 동안의 쓰기를 복제도 따라간다).
    let repl = opl_types::replication::replica_bitmap(Uuid::now_v7(), Uuid::now_v7());
    qemu_img(&["bitmap", "--add", path.to_str().unwrap(), &repl]);
    let (a, b) = (
        opl_types::backup::bitmap_name(Uuid::now_v7()),
        opl_types::backup::bitmap_name(Uuid::now_v7()),
    );

    storages
        .set_backup_bitmap(&pool, id, qcow2, &a)
        .await
        .unwrap();
    storages
        .set_backup_bitmap(&pool, id, qcow2, &b)
        .await
        .unwrap();
    // 다시 해도(같은 이름) 하나다.
    storages
        .set_backup_bitmap(&pool, id, qcow2, &b)
        .await
        .unwrap();
    let auto = vec!["auto".to_string()];
    assert_eq!(
        bitmaps(&path),
        [
            ("user".to_string(), auto.clone()),
            (repl.clone(), auto.clone()),
            (b.clone(), auto)
        ]
    );

    // 지운 뒤에는 스냅샷으로 되돌릴 수 있다(비트맵이 있으면 qemu-img가 거절한다, QEMU 11에서 확인).
    let tag = Uuid::now_v7().to_string();
    storages
        .snapshot_create(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    qemu_img(&["bitmap", "--remove", path.to_str().unwrap(), "user"]);
    // 되돌리기 전 지우기는 복제 비트맵도 덮는다(되돌린 디스크는 복제도 다음이 전체다).
    storages
        .drop_backup_bitmaps(&pool, id, qcow2)
        .await
        .unwrap();
    assert!(bitmaps(&path).is_empty());
    storages
        .snapshot_revert(&pool, id, qcow2, &tag)
        .await
        .unwrap();
    // 없어도 성공한다.
    storages
        .drop_backup_bitmaps(&pool, id, qcow2)
        .await
        .unwrap();

    let raw_id = Uuid::now_v7();
    storages
        .create_volume(&pool, raw_id, MIB, raw)
        .await
        .unwrap();
    storages
        .set_backup_bitmap(&pool, raw_id, raw, "opl-a")
        .await
        .unwrap();
    storages
        .drop_backup_bitmaps(&pool, raw_id, raw)
        .await
        .unwrap();
    let missing = storages
        .set_backup_bitmap(&pool, Uuid::now_v7(), qcow2, "opl-a")
        .await;
    assert!(
        matches!(missing, Err(DriverError::VolumeNotFound(_))),
        "{missing:?}"
    );
}

/// 두 이미지의 내용이 같은지(`qemu-img compare`, 형식은 추측)
fn same_contents(a: &Path, b: &Path) -> bool {
    std::process::Command::new("qemu-img")
        .args(["compare", "-q", a.to_str().unwrap(), b.to_str().unwrap()])
        .status()
        .unwrap()
        .success()
}

/// 증분 백업 체인 가져오기: 전체 백업과 바뀐 블록만 담은 backing 없는 qcow2를 체인으로 열어 원본과
/// 같은 새 볼륨(qcow2, raw)을 만든다. 증분 파일만으로는 원본이 아니다.
#[tokio::test]
async fn imports_a_backup_chain() {
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, out, dst_dir) = (
        tmp.path().join("src"),
        tmp.path().join("out"),
        tmp.path().join("dst"),
    );
    for dir in [&src_dir, &out, &dst_dir] {
        std::fs::create_dir(dir).unwrap();
    }
    let (src_pool, dst_pool) = (pool(&src_dir), pool(&dst_dir));
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)));
    let (qcow2, raw) = (ImageFormat::Qcow2, ImageFormat::Raw);

    let src = Uuid::now_v7();
    let src_path = storages
        .create_volume(&src_pool, src, 64 * MIB, qcow2)
        .await
        .unwrap();
    assert!(qemu_io(&src_path, "write -P 0xab 0 1M"));
    let source = storages
        .volume_source(&src_pool, src, qcow2, None)
        .await
        .unwrap();
    let full = out.join("full.qcow2");
    storages.export_volume(&source, false, &full).await.unwrap();

    // 바뀐 블록을 원본과 빈 증분 파일에 같이 쓴다(QEMU의 증분 백업처럼).
    let incr = out.join("incr.qcow2");
    storages
        .create_backup_target(&incr, 64 * MIB)
        .await
        .unwrap();
    let info = image_info(&incr);
    assert_eq!(info["format"], "qcow2");
    assert_eq!(info["virtual-size"], 64 * MIB);
    for cmd in ["write -P 0xcd 512k 64k", "write -P 0xef 32M 64k"] {
        assert!(qemu_io(&src_path, cmd));
        assert!(qemu_io(&incr, cmd));
    }
    for file in [&full, &incr] {
        storages
            .check_import_source(file, SourceFormat::Qcow2)
            .await
            .unwrap();
    }

    let chain = [incr.clone(), full.clone()];
    for format in [qcow2, raw] {
        let id = Uuid::now_v7();
        let path = storages
            .import_chain(&dst_pool, id, &chain, format)
            .await
            .unwrap();
        assert_eq!(path, volume_path(&dst_pool, id, format));
        assert!(same_contents(&path, &src_path));
    }
    assert!(!same_contents(&incr, &src_path));
    assert!(!same_contents(&full, &src_path));

    // 체인의 파일이 없으면 실패하고 볼륨을 남기지 않는다.
    let id = Uuid::now_v7();
    let broken = [incr.clone(), out.join("gone.qcow2")];
    let err = storages
        .import_chain(&dst_pool, id, &broken, qcow2)
        .await
        .unwrap_err();
    assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    assert!(!volume_path(&dst_pool, id, qcow2).exists());
}

/// 복제본 겹층: dir 풀의 qcow2·raw 볼륨을 backing으로 둔 겹층에 쓴 것만 합치면 볼륨에 들어가고 겹층은 지워진다.
/// 다시 합쳐도(겹층 없음) 같다. 겹층을 버리면 볼륨은 그대로다.
#[tokio::test]
async fn replica_overlays_commit_into_the_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storages = Storages::new("qemu-img", Arc::new(DirStorage::new("qemu-img", MNT)));
    for format in [ImageFormat::Qcow2, ImageFormat::Raw] {
        let id = Uuid::now_v7();
        let path = storages
            .create_volume(&pool, id, 4 * MIB, format)
            .await
            .unwrap();
        assert!(qemu_io_fmt(&path, format, "write -P 0x11 0 4M"));
        let overlay = tmp.path().join(format!("{id}-overlay.qcow2"));
        storages
            .create_overlay(&pool, id, format, &overlay)
            .await
            .unwrap();
        assert!(qemu_io(&overlay, "write -P 0x22 1M 64k"));
        // 겹층을 통해 읽으면 볼륨 위에 바뀐 것이다. 볼륨은 아직 그대로다.
        assert!(qemu_io(&overlay, "read -P 0x11 0 1M"));
        assert!(qemu_io_fmt(&path, format, "read -P 0x11 1M 64k"));
        storages
            .commit_overlay(&pool, id, format, &overlay)
            .await
            .unwrap();
        assert!(!overlay.exists());
        assert!(qemu_io_fmt(&path, format, "read -P 0x22 1M 64k"));
        assert!(qemu_io_fmt(&path, format, "read -P 0x11 0 1M"));
        storages
            .commit_overlay(&pool, id, format, &overlay)
            .await
            .unwrap();

        // 버린 겹층(끝나지 않은 회차)은 볼륨에 닿지 않는다.
        storages
            .create_overlay(&pool, id, format, &overlay)
            .await
            .unwrap();
        assert!(qemu_io(&overlay, "write -P 0x33 0 64k"));
        std::fs::remove_file(&overlay).unwrap();
        assert!(qemu_io_fmt(&path, format, "read -P 0x11 0 64k"));
    }
}

/// 연결 복제(파일 풀): 기반(raw·qcow2)을 상대 경로 backing으로 둔 qcow2 겹층이다. 복제는 기반의 내용을 읽고, 복제에
/// 써도 기반 파일은 바이트까지 그대로다. 풀 디렉터리를 다른 이름으로 옮겨도(노드마다 다른 NFS 마운트 지점) 열린다.
/// 전체 복사·내보내기는 backing 없는 전체 데이터다. 평탄화(`rebase`) 뒤에는 기반을 지워도 내용이 같고, 다시 해도
/// 된다. 내부 스냅샷이 있는 복제는 평탄화하지 않는다.
#[tokio::test]
async fn linked_clones_share_the_base_through_a_relative_backing_path() {
    for base_format in [ImageFormat::Raw, ImageFormat::Qcow2] {
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("a");
        std::fs::create_dir(&first).unwrap();
        let pool_a = pool(&first);
        let storage = DirStorage::new("qemu-img", MNT);
        let (base, clone) = (Uuid::now_v7(), Uuid::now_v7());
        let base_path = storage
            .create_volume(&pool_a, base, 16 * MIB, base_format)
            .await
            .unwrap();
        assert!(qemu_io_fmt(&base_path, base_format, "write -P 0x5a 0 1M"));
        let base_bytes = std::fs::read(&base_path).unwrap();

        let (path, format) = storage
            .link_clone(&pool_a, base, base_format, clone)
            .await
            .unwrap();
        assert_eq!(path, volume_path(&pool_a, clone, ImageFormat::Qcow2));
        assert_eq!(format, ImageFormat::Qcow2);
        let info = image_info(&path);
        let backing = format!(
            "{base}.{}",
            if base_format == ImageFormat::Raw {
                "raw"
            } else {
                "qcow2"
            }
        );
        assert_eq!(info["backing-filename"], backing.as_str(), "{info}");
        assert_eq!(info["virtual-size"], 16 * MIB, "{info}");
        // 만드는 동안의 임시 파일은 남지 않는다.
        let mut names: Vec<String> = std::fs::read_dir(&first)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut want = vec![backing.clone(), format!("{clone}.qcow2")];
        want.sort();
        assert_eq!(names, want);
        let err = storage
            .link_clone(&pool_a, base, base_format, clone)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == clone),
            "{err:?}"
        );

        assert!(qemu_io(&path, "read -P 0x5a 0 1M"));
        assert!(qemu_io(&path, "write -P 0x77 1M 1M"));
        assert_eq!(std::fs::read(&base_path).unwrap(), base_bytes);

        // 풀 디렉터리를 옮겨도 상대 backing으로 열린다.
        let second = tmp.path().join("b");
        std::fs::rename(&first, &second).unwrap();
        let pool_b = pool(&second);
        let path = volume_path(&pool_b, clone, ImageFormat::Qcow2);
        assert!(qemu_io(&path, "read -P 0x5a 0 1M"));
        assert!(qemu_io(&path, "read -P 0x77 1M 1M"));
        let info = storage
            .volume_info(&pool_b, clone, ImageFormat::Qcow2)
            .await
            .unwrap();
        assert_eq!(info.virtual_bytes, 16 * MIB);

        // 전체 복사와 내보내기는 기반 없이 같은 내용이다.
        let copy = Uuid::now_v7();
        storage
            .copy_volume(
                &pool_b,
                clone,
                ImageFormat::Qcow2,
                None,
                false,
                &pool_b,
                copy,
                ImageFormat::Qcow2,
            )
            .await
            .unwrap();
        let copy_path = volume_path(&pool_b, copy, ImageFormat::Qcow2);
        assert!(image_info(&copy_path).get("backing-filename").is_none());
        assert!(same_contents(&copy_path, &path));
        let src = storage
            .volume_source(&pool_b, clone, ImageFormat::Qcow2, None)
            .await
            .unwrap();
        let exported = tmp.path().join("export.qcow2");
        storage.export_volume(&src, false, &exported).await.unwrap();
        assert!(image_info(&exported).get("backing-filename").is_none());
        assert!(same_contents(&exported, &path));

        // 내부 스냅샷이 있으면 평탄화하지 않는다(스냅샷이 기반을 잃는다).
        storage
            .snapshot_create(&pool_b, clone, ImageFormat::Qcow2, "s1")
            .await
            .unwrap();
        let err = storage
            .flatten_volume(&pool_b, clone, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::InvalidSpec(ref m) if m.contains("snapshots")),
            "{err:?}"
        );
        storage
            .snapshot_delete(&pool_b, clone, ImageFormat::Qcow2, "s1")
            .await
            .unwrap();

        storage
            .flatten_volume(&pool_b, clone, ImageFormat::Qcow2)
            .await
            .unwrap();
        assert!(image_info(&path).get("backing-filename").is_none());
        storage
            .delete_volume(&pool_b, base, base_format)
            .await
            .unwrap();
        assert!(qemu_io(&path, "read -P 0x5a 0 1M"));
        assert!(qemu_io(&path, "read -P 0x77 1M 1M"));
        storage
            .flatten_volume(&pool_b, clone, ImageFormat::Qcow2)
            .await
            .unwrap();
        // 기반이 없으면 복제를 만들지 못한다.
        let err = storage
            .link_clone(&pool_b, base, base_format, Uuid::now_v7())
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeNotFound(v) if v == base),
            "{err:?}"
        );
    }
}

/// 평탄화가 끊겨도(qemu-img를 죽임) 옛 사슬이 그대로이므로 다시 하면 된다.
#[tokio::test]
async fn interrupted_flatten_can_be_redone() {
    let tmp = tempfile::tempdir().unwrap();
    let pool = pool(tmp.path());
    let storage = DirStorage::new("qemu-img", MNT);
    let (base, clone) = (Uuid::now_v7(), Uuid::now_v7());
    let base_path = storage
        .create_volume(&pool, base, 256 * MIB, ImageFormat::Raw)
        .await
        .unwrap();
    assert!(qemu_io_fmt(
        &base_path,
        ImageFormat::Raw,
        "write -P 0x42 0 192M"
    ));
    let (path, _) = storage
        .link_clone(&pool, base, ImageFormat::Raw, clone)
        .await
        .unwrap();
    assert!(qemu_io(&path, "write -P 0x24 200M 1M"));

    // 곧 끊는다(취소하면 qemu-img를 죽인다). 그사이 끝났을 수도 있다.
    let flatten = storage.flatten_volume(&pool, clone, ImageFormat::Qcow2);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(30), flatten).await;
    // 취소는 qemu-img에 SIGKILL을 보내고 거두기는 tokio가 뒤에서 한다. 죽은 프로세스가 이미지 잠금을 놓을 때까지 기다린다
    // (잠금이 남아 있으면 qemu-io가 열지 못한다).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !qemu_io(&path, "read -P 0x42 0 192M") {
        assert!(
            std::time::Instant::now() < deadline,
            "the interrupted flatten changed the data"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(qemu_io(&path, "read -P 0x24 200M 1M"));

    storage
        .flatten_volume(&pool, clone, ImageFormat::Qcow2)
        .await
        .unwrap();
    assert!(image_info(&path).get("backing-filename").is_none());
    std::fs::remove_file(&base_path).unwrap();
    assert!(qemu_io(&path, "read -P 0x42 0 192M"));
    assert!(qemu_io(&path, "read -P 0x24 200M 1M"));
    assert!(qemu_io(&path, "read -P 0 192M 8M"));
}
