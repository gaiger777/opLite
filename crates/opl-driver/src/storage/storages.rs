use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, convert_from, path_str, rbd, run_qemu_img,
};
use crate::error::DriverError;

/// 볼륨을 다루는 방식이 같은 풀 백엔드의 묶음.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 볼륨 파일(`dir`, `nfs`)
    File,
    LvmThin,
    Zfs,
    Rbd,
}

impl Kind {
    fn of(pool: &StoragePool) -> Self {
        match &pool.spec.backend {
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => Kind::File,
            PoolBackend::LvmThin { .. } => Kind::LvmThin,
            PoolBackend::Zfs { .. } => Kind::Zfs,
            PoolBackend::Rbd { .. } => Kind::Rbd,
        }
    }
}

/// 저장소 호출을 풀 백엔드 종류에 맞는 저장소로 보낸다. 백엔드가 다른 풀 사이의 복사는 여기서
/// 한다(대상 백엔드에 빈 볼륨을 만들고 `qemu-img convert -n`으로 원본을 쓴다).
pub struct Storages {
    qemu_img: PathBuf,
    /// 파일 풀(`dir`, `nfs`)
    file: Arc<dyn Storage>,
    /// `lvm-thin` 풀. 없으면 그 풀은 쓸 수 없다.
    lvm_thin: Option<Arc<dyn Storage>>,
    /// `zfs` 풀. 없으면 그 풀은 쓸 수 없다.
    zfs: Option<Arc<dyn Storage>>,
    /// `rbd` 풀. 없으면 그 풀은 쓸 수 없다.
    rbd: Option<Arc<dyn Storage>>,
}

impl Storages {
    pub fn new(qemu_img: impl Into<PathBuf>, file: Arc<dyn Storage>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            file,
            lvm_thin: None,
            zfs: None,
            rbd: None,
        }
    }

    /// `lvm-thin` 풀을 다룰 저장소를 정한다.
    pub fn with_lvm_thin(mut self, storage: Arc<dyn Storage>) -> Self {
        self.lvm_thin = Some(storage);
        self
    }

    /// `zfs` 풀을 다룰 저장소를 정한다.
    pub fn with_zfs(mut self, storage: Arc<dyn Storage>) -> Self {
        self.zfs = Some(storage);
        self
    }

    /// `rbd` 풀을 다룰 저장소를 정한다.
    pub fn with_rbd(mut self, storage: Arc<dyn Storage>) -> Self {
        self.rbd = Some(storage);
        self
    }

    fn backend(&self, pool: &StoragePool) -> Result<&dyn Storage, DriverError> {
        self.shared(pool).map(|b| &**b)
    }

    /// [`Self::backend`]의 저장소. 뒤에서 쓰도록 복제할 수 있다.
    fn shared(&self, pool: &StoragePool) -> Result<&Arc<dyn Storage>, DriverError> {
        match Kind::of(pool) {
            Kind::File => Ok(&self.file),
            Kind::LvmThin => self.lvm_thin.as_ref().ok_or_else(|| {
                DriverError::InvalidSpec("lvm-thin pools are not supported yet".into())
            }),
            Kind::Zfs => self
                .zfs
                .as_ref()
                .ok_or_else(|| DriverError::InvalidSpec("zfs pools are not supported yet".into())),
            Kind::Rbd => self
                .rbd
                .as_ref()
                .ok_or_else(|| DriverError::InvalidSpec("rbd pools are not supported yet".into())),
        }
    }

    /// 대상 백엔드에 형식 `format`의 빈 볼륨을 만들고 `src`를 써 넣는다. 쓰다 실패하면 만든
    /// 볼륨을 지운다. 그사이 취소되면 볼륨이 남는다(호출자가 만든 볼륨으로 적어 두고 정리한다).
    #[allow(clippy::too_many_arguments)]
    async fn write_into(
        &self,
        src: &VolumeSource,
        force_share: bool,
        size_bytes: u64,
        dst: &dyn Storage,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let zero = dst.target_reads_zero(dst_pool).await?;
        let path = dst
            .create_volume(dst_pool, dst_id, size_bytes, format)
            .await?;
        let written = async {
            // RBD 이미지는 경로가 없으므로 옵션 문자열로 준다(형식도 그 안에 있다).
            let image = rbd::pool_target(dst_pool, dst_id);
            let from = convert_from(src, force_share, image.is_none().then_some(format))?;
            // -n: 만들어 둔 볼륨(파일, 장치 또는 RBD 이미지)에 쓴다. 새 볼륨이 0으로 읽히면 0인
            // 부분은 쓰지 않는다(--target-is-zero).
            let mut args = vec!["convert", "-q", "-n"];
            if zero {
                args.push("--target-is-zero");
            }
            if image.is_some() {
                args.push("--target-image-opts");
            }
            args.extend(from.iter().map(String::as_str));
            let dst = match &image {
                Some(image) => rbd::image_opts(image, None),
                None => path_str(&path)?.to_string(),
            };
            args.push(&dst);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = dst.delete_volume(dst_pool, dst_id, format).await {
                tracing::warn!(volume = %dst_id, error = %del, "cannot remove partial copy");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for Storages {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.backend(pool)?
            .create_volume(pool, id, size_bytes, format)
            .await
    }

    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.backend(pool)?
            .import_volume(pool, id, src, src_format, format)
            .await
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        self.file.check_import_source(src, src_format).await
    }

    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.delete_volume(pool, id, format).await
    }

    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.backend(pool)?.activate_volume(pool, id, format).await
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        self.backend(pool)?.volume_info(pool, id, format).await
    }

    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        self.backend(pool)?.capacity(pool).await
    }

    async fn target_reads_zero(&self, pool: &StoragePool) -> Result<bool, DriverError> {
        self.backend(pool)?.target_reads_zero(pool).await
    }

    /// 다룰 저장소가 없는 풀은 지원하지 않는다.
    fn supports_snapshots(&self, pool: &StoragePool, format: ImageFormat) -> bool {
        self.backend(pool)
            .is_ok_and(|b| b.supports_snapshots(pool, format))
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_create(pool, id, format, tag)
            .await
    }

    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_many(pool, ids, format, tag)
            .await
    }

    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_revert(pool, id, format, tag)
            .await
    }

    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .snapshot_delete(pool, id, format, tag)
            .await
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        self.backend(pool)?.snapshot_tags(pool, id, format).await
    }

    async fn snapshot_tags_older_than(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        age: Duration,
    ) -> Result<Vec<String>, DriverError> {
        self.backend(pool)?
            .snapshot_tags_older_than(pool, id, format, age)
            .await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .resize_volume(pool, id, format, size_bytes)
            .await
    }

    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        self.backend(pool)?
            .volume_source(pool, id, format, tag)
            .await
    }

    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        self.backend(pool)?
            .release_source(pool, id, format, tag)
            .await
    }

    /// 같은 백엔드 사이면 그 저장소가 복사한다. 다르면 원본(`volume_source`)의 크기로 대상 볼륨을
    /// 만들고 `qemu-img convert -n`으로 원본을 써 넣은 뒤 원본을 내놓는다.
    async fn copy_volume(
        &self,
        src_pool: &StoragePool,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePool,
        dst_id: Uuid,
        dst_format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (src, dst) = (self.backend(src_pool)?, self.backend(dst_pool)?);
        if Kind::of(src_pool) == Kind::of(dst_pool) {
            return dst
                .copy_volume(
                    src_pool,
                    src_id,
                    format,
                    tag,
                    force_share,
                    dst_pool,
                    dst_id,
                    dst_format,
                )
                .await;
        }
        let source = src.volume_source(src_pool, src_id, format, tag).await?;
        // 읽는 동안 취소돼도 원본을 내놓는다. 대상은 원본(스냅샷이면 그때)의 크기로 만든다.
        let guard = Source::new(self.shared(src_pool)?, src_pool, src_id, format, tag);
        let size = source.size_bytes;
        let written = self
            .write_into(
                &source,
                force_share,
                size,
                dst,
                dst_pool,
                dst_id,
                dst_format,
            )
            .await;
        // 복사는 끝났으므로 내놓기가 실패해도 성공이다(활성화된 채 남을 뿐이다).
        guard.release().await;
        written
    }
}

/// [`Storage::volume_source`]로 읽을 수 있게 한 복사 원본. `release`로 내놓지 않고 버려지면(복사
/// future가 취소됨) 뒤에서 내놓는다.
struct Source {
    storage: Arc<dyn Storage>,
    pool: StoragePool,
    id: Uuid,
    format: ImageFormat,
    tag: Option<String>,
    released: bool,
}

impl Source {
    fn new(
        storage: &Arc<dyn Storage>,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Self {
        Self {
            storage: storage.clone(),
            pool: pool.clone(),
            id,
            format,
            tag: tag.map(str::to_string),
            released: false,
        }
    }

    /// 원본을 내놓는다. 복사는 끝났으므로 실패해도 로그만 남긴다(활성화된 채 남을 뿐이다).
    async fn release(mut self) {
        self.released = true;
        release(
            &*self.storage,
            &self.pool,
            self.id,
            self.format,
            self.tag.as_deref(),
        )
        .await;
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (storage, pool, tag) = (self.storage.clone(), self.pool.clone(), self.tag.take());
        let (id, format) = (self.id, self.format);
        rt.spawn(async move { release(&*storage, &pool, id, format, tag.as_deref()).await });
    }
}

async fn release(
    storage: &dyn Storage,
    pool: &StoragePool,
    id: Uuid,
    format: ImageFormat,
    tag: Option<&str>,
) {
    if let Err(e) = storage.release_source(pool, id, format, tag).await {
        tracing::warn!(volume = %id, error = %e, "cannot release copy source");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    type Log = Arc<Mutex<Vec<String>>>;

    /// 호출을 `{이름}.{메서드}`로 적는 저장소. 볼륨은 `{dir}/{id}`에 있다고 한다. 복사 원본의 크기는
    /// 2GiB이고(`volume_info`는 1GiB), 만든 볼륨의 크기를 `created`에 적는다.
    /// `fail`에 적은 메서드는 `ToolFailed`로 실패한다.
    struct Recorder {
        name: &'static str,
        dir: PathBuf,
        log: Log,
        fail: Vec<&'static str>,
        created: Mutex<Vec<u64>>,
        /// 새 볼륨이 0으로 읽히는지([`Storage::target_reads_zero`])
        reads_zero: bool,
    }

    impl Recorder {
        fn new(name: &'static str, dir: &Path, log: &Log) -> Self {
            Self {
                name,
                dir: dir.to_path_buf(),
                log: log.clone(),
                fail: vec![],
                created: Mutex::default(),
                reads_zero: true,
            }
        }

        fn failing(mut self, method: &'static str) -> Self {
            self.fail.push(method);
            self
        }

        fn call(&self, method: &'static str) -> Result<(), DriverError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("{}.{method}", self.name));
            if self.fail.contains(&method) {
                return Err(DriverError::ToolFailed {
                    tool: "fake",
                    stderr: method.into(),
                });
            }
            Ok(())
        }
    }

    #[async_trait]
    impl Storage for Recorder {
        async fn create_volume(
            &self,
            _: &StoragePool,
            id: Uuid,
            size_bytes: u64,
            _: ImageFormat,
        ) -> Result<PathBuf, DriverError> {
            self.call("create_volume")?;
            self.created.lock().unwrap().push(size_bytes);
            Ok(self.dir.join(id.to_string()))
        }
        async fn import_volume(
            &self,
            _: &StoragePool,
            id: Uuid,
            _: &Path,
            _: SourceFormat,
            _: ImageFormat,
        ) -> Result<PathBuf, DriverError> {
            self.call("import_volume")?;
            Ok(self.dir.join(id.to_string()))
        }
        async fn check_import_source(&self, _: &Path, _: SourceFormat) -> Result<(), DriverError> {
            self.call("check_import_source")
        }
        async fn delete_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("delete_volume")
        }
        async fn activate_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<(), DriverError> {
            self.call("activate_volume")
        }
        async fn volume_info(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<VolumeInfo, DriverError> {
            self.call("volume_info")?;
            Ok(VolumeInfo {
                virtual_bytes: 1 << 30,
                allocated_bytes: 0,
            })
        }
        async fn capacity(&self, _: &StoragePool) -> Result<Capacity, DriverError> {
            self.call("capacity")?;
            Ok(Capacity {
                total_bytes: 0,
                available_bytes: 0,
            })
        }
        fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
            self.call("supports_snapshots").is_ok()
        }
        async fn target_reads_zero(&self, _: &StoragePool) -> Result<bool, DriverError> {
            Ok(self.reads_zero)
        }
        async fn snapshot_create(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_create")
        }
        async fn snapshot_many(
            &self,
            _: &StoragePool,
            _: &[Uuid],
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_many")
        }
        async fn snapshot_revert(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_revert")
        }
        async fn snapshot_delete(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: &str,
        ) -> Result<(), DriverError> {
            self.call("snapshot_delete")
        }
        async fn snapshot_tags(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
        ) -> Result<Vec<String>, DriverError> {
            self.call("snapshot_tags")?;
            Ok(vec![])
        }
        async fn snapshot_tags_older_than(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: Duration,
        ) -> Result<Vec<String>, DriverError> {
            self.call("snapshot_tags_older_than")?;
            Ok(vec![])
        }
        async fn resize_volume(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: u64,
        ) -> Result<(), DriverError> {
            self.call("resize_volume")
        }
        async fn volume_source(
            &self,
            pool: &StoragePool,
            id: Uuid,
            format: ImageFormat,
            tag: Option<&str>,
        ) -> Result<VolumeSource, DriverError> {
            self.call("volume_source")?;
            // RBD 풀이면 RBD 이미지다.
            let image = rbd::pool_target(pool, id);
            Ok(VolumeSource {
                path: match image {
                    Some(_) => PathBuf::new(),
                    None => self.dir.join(id.to_string()),
                },
                format,
                snapshot: tag.map(str::to_string),
                size_bytes: 2 << 30,
                rbd: image,
            })
        }
        async fn release_source(
            &self,
            _: &StoragePool,
            _: Uuid,
            _: ImageFormat,
            _: Option<&str>,
        ) -> Result<(), DriverError> {
            self.call("release_source")
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
            self.call("copy_volume")
        }
    }

    fn pool(backend: PoolBackend) -> StoragePool {
        Resource::new(
            "p",
            StoragePoolSpec {
                backend,
                nodes: vec![],
                shared: false,
            },
        )
    }

    fn dir_pool() -> StoragePool {
        pool(PoolBackend::Dir {
            path: "/pools/local".into(),
        })
    }

    fn nfs_pool() -> StoragePool {
        pool(PoolBackend::Nfs {
            server: "nas".into(),
            export: "/srv/vm".into(),
            options: None,
        })
    }

    fn lvm_pool() -> StoragePool {
        pool(PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "tp".into(),
        })
    }

    fn zfs_pool() -> StoragePool {
        pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        })
    }

    fn rbd_pool() -> StoragePool {
        pool(PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: Some("opl".into()),
            conf: None,
        })
    }

    /// 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜 qemu-img. `fail`이면 실패한다.
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        crate::storage::tests::write_script(
            &path,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {args:?}\n{end}\n"),
        );
        path
    }

    fn taken(log: &Log) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    /// 파일 풀(dir, nfs)은 파일 저장소로, lvm-thin 풀은 LVM 저장소로, zfs 풀은 ZFS 저장소로, rbd
    /// 풀은 RBD 저장소로 간다.
    #[tokio::test]
    async fn calls_go_to_the_pool_backend() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)))
        .with_rbd(Arc::new(Recorder::new("rbd", "".as_ref(), &log)));
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        for (p, name) in [
            (dir_pool(), "file"),
            (nfs_pool(), "file"),
            (lvm_pool(), "lvm"),
            (zfs_pool(), "zfs"),
            (rbd_pool(), "rbd"),
        ] {
            storages.create_volume(&p, id, 1, f).await.unwrap();
            let src = Path::new("/up/x.vmdk");
            storages
                .import_volume(&p, id, src, SourceFormat::Vmdk, f)
                .await
                .unwrap();
            storages.delete_volume(&p, id, f).await.unwrap();
            storages.activate_volume(&p, id, f).await.unwrap();
            storages.volume_info(&p, id, f).await.unwrap();
            storages.capacity(&p).await.unwrap();
            assert!(storages.supports_snapshots(&p, f));
            storages.snapshot_create(&p, id, f, "t").await.unwrap();
            storages.snapshot_many(&p, &[id], f, "t").await.unwrap();
            storages.snapshot_revert(&p, id, f, "t").await.unwrap();
            storages.snapshot_delete(&p, id, f, "t").await.unwrap();
            storages.snapshot_tags(&p, id, f).await.unwrap();
            storages
                .snapshot_tags_older_than(&p, id, f, Duration::from_secs(60))
                .await
                .unwrap();
            storages.resize_volume(&p, id, f, 2).await.unwrap();
            storages.volume_source(&p, id, f, None).await.unwrap();
            storages.release_source(&p, id, f, None).await.unwrap();
            // 같은 백엔드 사이 복사는 그 저장소가 한다.
            storages
                .copy_volume(&p, id, f, None, false, &p, Uuid::now_v7(), f)
                .await
                .unwrap();
            let methods = [
                "create_volume",
                "import_volume",
                "delete_volume",
                "activate_volume",
                "volume_info",
                "capacity",
                "supports_snapshots",
                "snapshot_create",
                "snapshot_many",
                "snapshot_revert",
                "snapshot_delete",
                "snapshot_tags",
                "snapshot_tags_older_than",
                "resize_volume",
                "volume_source",
                "release_source",
                "copy_volume",
            ];
            let want: Vec<String> = methods.iter().map(|m| format!("{name}.{m}")).collect();
            assert_eq!(taken(&log), want);
        }
        // 올린 파일 검사는 풀과 상관없이 파일 저장소가 한다.
        storages
            .check_import_source(Path::new("/up/x.vmdk"), SourceFormat::Vmdk)
            .await
            .unwrap();
        assert_eq!(taken(&log), ["file.check_import_source"]);
        // dir과 nfs는 같은 백엔드다.
        storages
            .copy_volume(&dir_pool(), id, f, None, false, &nfs_pool(), id, f)
            .await
            .unwrap();
        assert_eq!(taken(&log), ["file.copy_volume"]);
    }

    /// LVM 저장소가 없으면 lvm-thin 풀은 쓸 수 없다.
    #[tokio::test]
    async fn lvm_thin_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        );
        let (p, id, f) = (lvm_pool(), Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains("not supported"), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.delete_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        unsupported(storages.volume_source(&p, id, f, None).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&dir_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// ZFS 저장소가 없으면 zfs 풀은 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn zfs_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let p = pool(PoolBackend::Zfs {
            dataset: "tank/opl".into(),
        });
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "zfs pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&lvm_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// RBD 저장소가 없으면 rbd 풀은 쓸 수 없다(다른 저장소로 보내지 않는다).
    #[tokio::test]
    async fn rbd_pool_without_backend_is_unsupported() {
        let log = Log::default();
        let storages = Storages::new(
            "qemu-img",
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new("zfs", "/z".as_ref(), &log)));
        let p = pool(PoolBackend::Rbd {
            pool: "rbd".into(),
            namespace: None,
            user: None,
            conf: None,
        });
        let (id, f) = (Uuid::nil(), ImageFormat::Raw);
        let unsupported = |e: DriverError| match e {
            DriverError::InvalidSpec(msg) => {
                assert_eq!(msg, "rbd pools are not supported yet")
            }
            other => panic!("unexpected error: {other:?}"),
        };
        unsupported(storages.create_volume(&p, id, 1, f).await.unwrap_err());
        unsupported(storages.activate_volume(&p, id, f).await.unwrap_err());
        unsupported(storages.capacity(&p).await.unwrap_err());
        assert!(!storages.supports_snapshots(&p, f));
        unsupported(
            storages
                .copy_volume(&zfs_pool(), id, f, None, false, &p, id, f)
                .await
                .unwrap_err(),
        );
        assert_eq!(taken(&log), Vec::<String>::new());
    }

    /// 백엔드가 다르면 대상에 빈 볼륨을 만들고 원본을 써 넣은 뒤 원본을 내놓는다.
    #[tokio::test]
    async fn copy_between_backends_converts_into_new_volume() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/dev/vg0".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);

        // 실행 중인 VM의 qcow2 스냅샷을 raw로 바꿔 읽는다. 태그의 쉼표는 옵션 문자열에서 `,,`로 쓴다.
        storages
            .copy_volume(
                &dir_pool(),
                src,
                qcow2,
                Some("a,b"),
                true,
                &lvm_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "file.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/f/{src}"), format!("/dev/vg0/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "raw",
                &src_path,
                &dst_path,
            ]
        );

        // 반대 방향, 지금 내용
        storages
            .copy_volume(&lvm_pool(), src, raw, None, false, &nfs_pool(), dst, raw)
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/dev/vg0/{src}"), format!("/f/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "raw",
                "-O",
                "raw",
                &src_path,
                &dst_path,
            ]
        );
    }

    /// LVM과 ZFS는 다른 백엔드다(원본을 읽어 대상에 새로 써 넣는다).
    #[tokio::test]
    async fn copy_between_lvm_and_zfs_converts_into_new_volume() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/dev/vg0".as_ref(), &log)))
        .with_zfs(Arc::new(Recorder::new(
            "zfs",
            "/dev/zvol/tank/opl".as_ref(),
            &log,
        )));
        let (src, dst, raw) = (Uuid::now_v7(), Uuid::now_v7(), ImageFormat::Raw);
        storages
            .copy_volume(
                &lvm_pool(),
                src,
                raw,
                Some("t"),
                false,
                &zfs_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "zfs.create_volume",
                "lvm.release_source",
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let dst_path = format!("/dev/zvol/tank/opl/{dst}");
        assert_eq!(args.lines().last(), Some(dst_path.as_str()));
    }

    /// RBD 이미지는 경로가 없으므로 `qemu-img`에 옵션 문자열로 준다: 대상은 `--target-image-opts`
    /// (`-O` 없이), 원본은 `--image-opts`(스냅샷도 그 안에)다.
    #[tokio::test]
    async fn copy_to_and_from_rbd_uses_image_opts() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_rbd(Arc::new(Recorder::new("rbd", "".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);

        storages
            .copy_volume(
                &dir_pool(),
                src,
                qcow2,
                Some("t"),
                true,
                &rbd_pool(),
                dst,
                raw,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "rbd.create_volume",
                "file.release_source",
            ]
        );
        let (src_path, to) = (
            format!("/f/{src}"),
            format!("driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{dst},file.user=opl"),
        );
        assert_eq!(
            std::fs::read_to_string(tools.path().join("args"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=t",
                &src_path,
                &to,
            ]
        );

        storages
            .copy_volume(
                &rbd_pool(),
                src,
                raw,
                Some("t"),
                false,
                &nfs_pool(),
                dst,
                qcow2,
            )
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "rbd.volume_source",
                "file.create_volume",
                "rbd.release_source",
            ]
        );
        let (from, dst_path) = (
            format!(
                "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{src},file.snapshot=t,file.user=opl"
            ),
            format!("/f/{dst}"),
        );
        assert_eq!(
            std::fs::read_to_string(tools.path().join("args"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--image-opts",
                "-O",
                "qcow2",
                &from,
                &dst_path,
            ]
        );
    }

    /// 써 넣다 실패하면 만든 볼륨을 지우고, 원본은 어느 경우에도 내놓는다.
    #[tokio::test]
    async fn failed_copy_between_backends_cleans_up() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;
        let (from, to) = (dir_pool(), lvm_pool());

        let storages = Storages::new(
            fake_qemu_img(tools.path(), true),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let err = storages
            .copy_volume(&from, src, raw, None, false, &to, dst, raw)
            .await
            .unwrap_err();
        match err {
            DriverError::ToolFailed { tool, stderr } => {
                assert_eq!((tool, stderr.as_str()), ("qemu-img", "broken"))
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "lvm.delete_volume",
                "file.release_source",
            ]
        );

        // 대상을 만들지 못하면(이미 있음 등) 지우지 않는다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(
            Recorder::new("lvm", "/l".as_ref(), &log).failing("create_volume"),
        ));
        storages
            .copy_volume(&from, src, raw, None, false, &to, dst, raw)
            .await
            .unwrap_err();
        assert_eq!(
            taken(&log),
            [
                "file.volume_source",
                "lvm.create_volume",
                "file.release_source",
            ]
        );

        // 원본을 읽을 수 없으면 아무것도 만들지 않는다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log).failing("volume_source")),
        )
        .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        storages
            .copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw)
            .await
            .unwrap_err();
        assert_eq!(taken(&log), ["file.volume_source"]);

        // 복사가 끝났으면 원본을 내놓지 못해도 성공이다.
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(
            Recorder::new("lvm", "/l".as_ref(), &log).failing("release_source"),
        ));
        storages
            .copy_volume(&to, src, raw, None, false, &from, dst, raw)
            .await
            .unwrap();
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
    }

    /// 대상은 원본(`volume_source`, 스냅샷이면 그때)의 크기로 만든다. 볼륨을 되돌려 지금이 더 작아도
    /// 스냅샷의 내용이 다 담긴다.
    #[tokio::test]
    async fn copy_between_backends_uses_the_source_size() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let lvm = Arc::new(Recorder::new("lvm", "/l".as_ref(), &log));
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(lvm.clone());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (raw, from, to) = (ImageFormat::Raw, dir_pool(), lvm_pool());
        storages
            .copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw)
            .await
            .unwrap();
        assert_eq!(*lvm.created.lock().unwrap(), [2 << 30]);
    }

    /// 복사가 취소돼도(future가 버려짐) 원본을 뒤에서 내놓는다.
    #[tokio::test]
    async fn cancelled_copy_between_backends_releases_the_source() {
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        crate::storage::tests::write_script(&slow, "#!/bin/sh\nsleep 5\n");
        let log = Log::default();
        let storages = Storages::new(slow, Arc::new(Recorder::new("file", "/f".as_ref(), &log)))
            .with_lvm_thin(Arc::new(Recorder::new("lvm", "/l".as_ref(), &log)));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let (from, to) = (lvm_pool(), dir_pool());
        let copy = storages.copy_volume(&from, src, raw, Some("t"), false, &to, dst, raw);
        let res = tokio::time::timeout(std::time::Duration::from_millis(300), copy).await;
        assert!(res.is_err(), "copy should still be running");
        for _ in 0..200 {
            if log
                .lock()
                .unwrap()
                .iter()
                .any(|c| c == "lvm.release_source")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            taken(&log),
            [
                "lvm.volume_source",
                "file.create_volume",
                "lvm.release_source",
            ]
        );
    }

    /// 백엔드 사이 복사는 대상 백엔드가 새 볼륨이 0으로 읽힌다고 하지 않으면(0으로 채우지 않는 thin
    /// pool) `--target-is-zero`를 쓰지 않는다.
    #[tokio::test]
    async fn copy_between_backends_asks_the_target_whether_it_reads_zero() {
        let tools = tempfile::tempdir().unwrap();
        let log = Log::default();
        let lvm = Recorder {
            reads_zero: false,
            ..Recorder::new("lvm", "/l".as_ref(), &log)
        };
        let storages = Storages::new(
            fake_qemu_img(tools.path(), false),
            Arc::new(Recorder::new("file", "/f".as_ref(), &log)),
        )
        .with_lvm_thin(Arc::new(lvm));
        assert!(storages.target_reads_zero(&dir_pool()).await.unwrap());
        assert!(!storages.target_reads_zero(&lvm_pool()).await.unwrap());
        let (src, dst, raw) = (Uuid::now_v7(), Uuid::now_v7(), ImageFormat::Raw);
        let source = VolumeSource {
            path: format!("/f/{src}").into(),
            format: raw,
            snapshot: None,
            size_bytes: 1 << 20,
            rbd: None,
        };
        let target = storages.backend(&lvm_pool()).unwrap();
        storages
            .write_into(&source, false, 1 << 20, target, &lvm_pool(), dst, raw)
            .await
            .unwrap();
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (src_path, dst_path) = (format!("/f/{src}"), format!("/l/{dst}"));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert", "-q", "-n", "-f", "raw", "-O", "raw", &src_path, &dst_path
            ]
        );
    }
}
