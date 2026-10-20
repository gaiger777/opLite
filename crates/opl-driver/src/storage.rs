use std::path::{Path, PathBuf};

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;

/// 가져올 원본 이미지 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Vmdk,
    Qcow2,
    Raw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeInfo {
    pub virtual_bytes: u64,
    pub allocated_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// 볼륨 파일의 생성, 가져오기, 삭제. 볼륨은 풀 안에서 id로 식별한다.
#[async_trait]
pub trait Storage: Send + Sync {
    async fn create_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// 외부 이미지(VMDK 등)를 변환해 새 볼륨으로 만든다. 실패하면 만들다 만 파일을 남기지 않는다.
    async fn import_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    async fn delete_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError>;

    /// VM이 실행 중이어도 조회할 수 있다.
    async fn volume_info(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError>;

    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError>;
}

/// `dir` 풀: 디렉터리 안에 `{id}.qcow2` / `{id}.raw` 파일을 두고 `qemu-img`로 다룬다.
pub struct DirStorage {
    qemu_img: PathBuf,
}

impl DirStorage {
    pub fn new(qemu_img: impl Into<PathBuf>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
        }
    }

    pub fn volume_path(pool: &StoragePoolSpec, id: Uuid, format: ImageFormat) -> PathBuf {
        let PoolBackend::Dir { path } = &pool.backend;
        path.join(format!("{id}.{}", fmt_name(format)))
    }

    async fn qemu_img(&self, args: &[&str]) -> Result<Vec<u8>, DriverError> {
        let out = tokio::process::Command::new(&self.qemu_img)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            return Err(DriverError::ToolFailed {
                tool: "qemu-img",
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(out.stdout)
    }
}

fn fmt_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    }
}

fn src_fmt_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Vmdk => "vmdk",
        SourceFormat::Qcow2 => "qcow2",
        SourceFormat::Raw => "raw",
    }
}

fn path_str(p: &Path) -> Result<&str, DriverError> {
    p.to_str()
        .ok_or_else(|| DriverError::InvalidSpec(format!("path is not UTF-8: {p:?}")))
}

#[async_trait]
impl Storage for DirStorage {
    async fn create_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.to_string();
        self.qemu_img(&[
            "create",
            "-q",
            "-f",
            fmt_name(format),
            path_str(&path)?,
            &size,
        ])
        .await?;
        Ok(path)
    }

    async fn import_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let args = [
            "convert",
            "-q",
            "-f",
            src_fmt_name(src_format),
            "-O",
            fmt_name(format),
            path_str(src)?,
            path_str(&path)?,
        ];
        if let Err(e) = self.qemu_img(&args).await {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        Ok(path)
    }

    async fn delete_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        match std::fs::remove_file(Self::volume_path(pool, id, format)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(DriverError::VolumeNotFound(id))
            }
            other => Ok(other?),
        }
    }

    async fn volume_info(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let path = Self::volume_path(pool, id, format);
        if !path.exists() {
            return Err(DriverError::VolumeNotFound(id));
        }
        // -U: 실행 중인 VM이 잡고 있는 이미지 잠금을 무시하고 읽기만 한다.
        let out = self
            .qemu_img(&[
                "info",
                "-U",
                "--output=json",
                "-f",
                fmt_name(format),
                path_str(&path)?,
            ])
            .await?;
        let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })?;
        let field = |k: &str| json.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(VolumeInfo {
            virtual_bytes: field("virtual-size"),
            allocated_bytes: field("actual-size"),
        })
    }

    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError> {
        let PoolBackend::Dir { path } = &pool.backend;
        Ok(Capacity {
            total_bytes: fs4::total_space(path)?,
            available_bytes: fs4::available_space(path)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(path: &str) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Dir { path: path.into() },
            nodes: vec![],
            shared: false,
        }
    }

    #[test]
    fn volume_path_uses_id_and_format() {
        let id = Uuid::nil();
        assert_eq!(
            DirStorage::volume_path(&pool("/pools/local"), id, ImageFormat::Qcow2),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.qcow2")
        );
        assert_eq!(
            DirStorage::volume_path(&pool("/pools/local"), id, ImageFormat::Raw),
            PathBuf::from("/pools/local/00000000-0000-0000-0000-000000000000.raw")
        );
    }

    #[tokio::test]
    async fn tool_failure_carries_stderr() {
        let storage = DirStorage::new("/bin/sh");
        // `sh create ...`는 파일이 없어 실패하고 stderr를 남긴다.
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let err = storage
            .create_volume(&p, Uuid::nil(), 1 << 20, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        match err {
            DriverError::ToolFailed { tool, stderr } => {
                assert_eq!(tool, "qemu-img");
                assert!(!stderr.is_empty());
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_missing_volume_is_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = DirStorage::new("qemu-img");
        let err = storage
            .delete_volume(
                &pool(tmp.path().to_str().unwrap()),
                Uuid::nil(),
                ImageFormat::Raw,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)));
    }

    #[tokio::test]
    async fn capacity_reports_filesystem_space() {
        let tmp = tempfile::tempdir().unwrap();
        let cap = DirStorage::new("qemu-img")
            .capacity(&pool(tmp.path().to_str().unwrap()))
            .await
            .unwrap();
        assert!(cap.total_bytes > 0);
        assert!(cap.available_bytes <= cap.total_bytes);
    }
}
