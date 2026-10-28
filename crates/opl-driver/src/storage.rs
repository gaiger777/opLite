use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePoolSpec};
use serde_json::Value;
use uuid::Uuid;

use crate::error::DriverError;

/// 올린 이미지를 검사하는 `qemu-img info`가 멈추면(이상한 extent 등) 무한정 기다리지 않는다.
const INFO_TIMEOUT: Duration = Duration::from_secs(60);

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
/// 같은 볼륨 id에 대한 호출은 호출자가 직렬화한다.
#[async_trait]
pub trait Storage: Send + Sync {
    /// 이미 있으면 `VolumeExists`. qemu-img는 크기를 512바이트 단위로 올림하므로
    /// 실제 크기는 `VolumeInfo::virtual_bytes`로 확인한다.
    async fn create_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// 외부 이미지(VMDK 등)를 변환해 새 볼륨으로 만든다. 실패하면 만들다 만 파일을 남기지 않는다.
    ///
    /// `src`와 그것이 참조하는 VMDK extent 파일은 믿을 수 있는 경로여야 한다.
    /// monolithicFlat VMDK 디스크립터는 호스트의 임의 파일을 extent로 가리킬 수 있다.
    /// 사용자가 올린 파일을 검증하는 것은 API(계획 5)의 책임이다.
    async fn import_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError>;

    /// `src`가 다른 파일을 참조하지 않는 이미지 하나인지 확인한다(VMDK extent, backing file).
    /// 사용자가 올린 파일을 `import_volume`에 넘기기 전에 부른다.
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError>;

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

    /// 이 백엔드가 `format` 볼륨의 스냅샷을 지원하는지. 지원하지 않는 형식에 스냅샷 메서드를
    /// 부르면 `InvalidSpec`.
    fn supports_snapshots(&self, format: ImageFormat) -> bool;

    /// 정지한 볼륨에 스냅샷 `tag`를 만든다. 같은 태그가 이미 있으면 `InvalidSpec`.
    async fn snapshot_create(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 정지한 볼륨의 내용을 스냅샷 `tag` 때로 되돌린다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 정지한 볼륨에서 스냅샷 `tag`를 지운다. 태그가 없으면 성공(다시 시도해도 된다).
    async fn snapshot_delete(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError>;

    /// 볼륨에 있는 스냅샷 태그들. VM이 실행 중이어도 조회할 수 있다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError>;

    /// 정지한 볼륨의 크기를 `size_bytes`로 늘린다. 줄이지는 않는다(실패한다).
    /// 실제 크기는 `volume_info`로 확인한다.
    async fn resize_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError>;

    /// 볼륨을 같은 형식의 새 볼륨 `dst_id`로 복사한다. `tag`가 있으면 그 스냅샷 때의 내용을,
    /// 없으면 지금 내용을 복사한다. `force_share`는 실행 중인 VM이 쥔 볼륨의 스냅샷을 읽을 때만
    /// 쓴다(스냅샷 데이터는 바뀌지 않는다). 대상이 이미 있으면 `VolumeExists`.
    /// 실패하면 만들다 만 파일을 남기지 않는다.
    #[allow(clippy::too_many_arguments)]
    async fn copy_volume(
        &self,
        src_pool: &StoragePoolSpec,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError>;
}

/// `dir` 풀: 디렉터리 안에 `{id}.qcow2` / `{id}.raw` 파일을 두고 `qemu-img`로 다룬다.
/// 풀 디렉터리는 hard link를 지원하는 파일시스템이어야 한다.
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
        let root = match &pool.backend {
            PoolBackend::Dir { path } => path.as_path(),
            // 아직 지원하지 않는다. 드라이버는 pool_dir에서, reconciler는 디스크를 풀 때 거절한다.
            PoolBackend::Nfs { .. } => Path::new(""),
        };
        root.join(format!("{id}.{}", fmt_name(format)))
    }

    async fn qemu_img(&self, args: &[&str]) -> Result<Vec<u8>, DriverError> {
        let out = tokio::process::Command::new(&self.qemu_img)
            .args(args)
            .stdin(std::process::Stdio::null())
            // 호출자가 취소하면 qemu-img도 끝낸다.
            .kill_on_drop(true)
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

    /// 있는 볼륨의 경로. 없으면 `VolumeNotFound`.
    fn existing_path(
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        pool_dir(pool)?;
        let path = Self::volume_path(pool, id, format);
        if !path.exists() {
            return Err(DriverError::VolumeNotFound(id));
        }
        Ok(path)
    }

    /// 스냅샷을 다룰 볼륨의 경로. 형식이 스냅샷을 지원해야 한다.
    fn snapshot_path(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        if !self.supports_snapshots(format) {
            return Err(DriverError::InvalidSpec(format!(
                "{} volumes do not support snapshots",
                fmt_name(format)
            )));
        }
        Self::existing_path(pool, id, format)
    }

    /// `qemu-img info`의 JSON 출력.
    async fn info_json(&self, path: &Path, format: ImageFormat) -> Result<Value, DriverError> {
        // -U: 실행 중인 VM이 잡고 있는 이미지 잠금을 무시하고 읽기만 한다.
        let out = self
            .qemu_img(&[
                "info",
                "-U",
                "--output=json",
                "-f",
                fmt_name(format),
                path_str(path)?,
            ])
            .await?;
        serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })
    }

    async fn tags_at(&self, path: &Path, format: ImageFormat) -> Result<Vec<String>, DriverError> {
        let json = self.info_json(path, format).await?;
        // 스냅샷이 없으면 snapshots 항목이 없다.
        let Some(snapshots) = json.get("snapshots") else {
            return Ok(Vec::new());
        };
        snapshots
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| {
                s.get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| DriverError::ToolFailed {
                        tool: "qemu-img",
                        stderr: format!("info output has a snapshot without name: {s}"),
                    })
            })
            .collect()
    }

    /// 풀 안의 임시 파일(`.{id}.{fmt}.{난수}.part`)에 이미지를 만든 뒤 `path`로 하드 링크한다. qemu-img는 기존 파일을
    /// 경고 없이 덮어쓰므로, 같은 볼륨을 동시에 만들어도 먼저 생긴 파일을 덮어쓰지 않게 한다.
    /// 출력 경로는 `before`와 `after` 인자 사이에 들어간다.
    async fn write_new(
        &self,
        id: Uuid,
        path: &Path,
        before: &[&str],
        after: &[&str],
    ) -> Result<(), DriverError> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        // 호출마다 고유한 이름을 쓴다. opld가 죽은 뒤에도 이전 시도의 qemu-img가 같은 이름의
        // 파일을 쓰고 있을 수 있고, qemu-img는 기존 파일을 잘라 덮어쓰므로 이름을 공유하면
        // 손상된 볼륨이 성공으로 게시될 수 있다. 크래시로 남은 .part는 용량만 차지한다.
        let tmp = TempFile(path.with_file_name(format!(".{name}.{}.part", Uuid::new_v4())));
        let mut args = before.to_vec();
        args.push(path_str(&tmp.0)?);
        args.extend_from_slice(after);
        self.qemu_img(&args).await?;
        match std::fs::hard_link(&tmp.0, path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(DriverError::VolumeExists(id))
            }
            other => Ok(other?),
        }
    }
}

/// drop되면 파일을 지운다. 실패하거나 취소돼도 임시 파일이 남지 않게 한다.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// qemu-img는 상대 경로를 `json:{...}`, `nbd:` 같은 프로토콜로 해석할 수 있으므로 절대 경로만 받는다.
fn require_absolute(p: &Path) -> Result<(), DriverError> {
    if p.is_absolute() {
        Ok(())
    } else {
        Err(DriverError::InvalidSpec(format!(
            "path must be absolute: {p:?}"
        )))
    }
}

fn pool_dir(pool: &StoragePoolSpec) -> Result<&Path, DriverError> {
    match &pool.backend {
        PoolBackend::Dir { path } => {
            require_absolute(path)?;
            Ok(path)
        }
        PoolBackend::Nfs { .. } => Err(DriverError::InvalidSpec(
            "nfs pools are not supported yet".into(),
        )),
    }
}

fn fmt_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Qcow2 => "qcow2",
        ImageFormat::Raw => "raw",
    }
}

/// `--image-opts`로 줄 옵션 문자열. `qemu-img snapshot`은 QEMU 8.2에서 `-f`를 받지 않으므로
/// 형식을 추측하지 않게 드라이버를 이렇게 정한다. 옵션 값 안의 `,`는 `,,`로 쓴다.
fn image_opts(path: &Path, format: ImageFormat) -> Result<String, DriverError> {
    Ok(format!(
        "driver={},file.filename={}",
        fmt_name(format),
        path_str(path)?.replace(',', ",,")
    ))
}

fn src_fmt_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Vmdk => "vmdk",
        SourceFormat::Qcow2 => "qcow2",
        SourceFormat::Raw => "raw",
    }
}

/// `qemu-img info`가 다른 파일이나 연결을 열기 전에 헤더만 보고 거부한다.
/// info는 보고하려고 VMDK extent와 qcow2 외부 데이터 파일을 실제로 연다.
async fn check_source_header(src: &Path, src_format: SourceFormat) -> Result<(), DriverError> {
    use tokio::io::AsyncReadExt;

    let mut header = Vec::with_capacity(80);
    tokio::fs::File::open(src)
        .await?
        .take(80)
        .read_to_end(&mut header)
        .await?;
    match src_format {
        // 텍스트 디스크립터는 다른 파일만 가리킬 수 있다.
        SourceFormat::Vmdk if header.starts_with(b"# Disk DescriptorFile") => {
            Err(DriverError::InvalidSpec(
                "descriptor-only VMDK cannot be imported; \
                 upload a monolithicSparse or streamOptimized VMDK"
                    .into(),
            ))
        }
        // 헤더는 빅엔디언이다. 매직이 틀리면 qemu-img가 거부한다.
        SourceFormat::Qcow2 if header.len() >= 16 && header.starts_with(b"QFI\xfb") => {
            let be32 = |at: usize| u32::from_be_bytes(header[at..at + 4].try_into().unwrap());
            let be64 = |at: usize| u64::from_be_bytes(header[at..at + 8].try_into().unwrap());
            if be64(8) != 0 {
                return Err(DriverError::InvalidSpec(
                    "qcow2 image has a backing file".into(),
                ));
            }
            // incompatible_features bit 2: 외부 데이터 파일 (version 3부터)
            if be32(4) >= 3 && header.len() >= 80 && be64(72) & 4 != 0 {
                return Err(DriverError::InvalidSpec(
                    "qcow2 image has an external data file".into(),
                ));
            }
            Ok(())
        }
        SourceFormat::Vmdk | SourceFormat::Qcow2 | SourceFormat::Raw => Ok(()),
    }
}

/// `qemu-img info` 출력에서 이미지가 여는(열 수 있는) 파일 이름을 모두 모은다.
fn referenced_files(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(map) => {
            for (k, v) in map {
                match (k.as_str(), v) {
                    (
                        "filename" | "backing-filename" | "full-backing-filename" | "data-file",
                        Value::String(f),
                    ) => out.push(f.clone()),
                    _ => referenced_files(v, out),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| referenced_files(v, out)),
        _ => {}
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
        pool_dir(pool)?;
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.to_string();
        self.write_new(
            id,
            &path,
            &["create", "-q", "-f", fmt_name(format)],
            &[&size],
        )
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
        pool_dir(pool)?;
        require_absolute(src)?;
        let path = Self::volume_path(pool, id, format);
        if path.exists() {
            return Err(DriverError::VolumeExists(id));
        }
        let before = [
            "convert",
            "-q",
            "-f",
            src_fmt_name(src_format),
            "-O",
            fmt_name(format),
            path_str(src)?,
        ];
        self.write_new(id, &path, &before, &[]).await?;
        Ok(path)
    }

    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        require_absolute(src)?;
        check_source_header(src, src_format).await?;
        let src = path_str(src)?;
        let args = ["info", "--output=json", "-f", src_fmt_name(src_format), src];
        // 시간이 지나 future가 버려지면 kill_on_drop이 qemu-img를 끝낸다.
        let out = tokio::time::timeout(INFO_TIMEOUT, self.qemu_img(&args))
            .await
            .map_err(|_| DriverError::Timeout("qemu-img info"))??;
        let json: Value = serde_json::from_slice(&out).map_err(|e| DriverError::ToolFailed {
            tool: "qemu-img",
            stderr: format!("unparseable info output: {e}"),
        })?;
        // 이미지 자신과 그 하위 노드(extent 등)가 가리키는 파일이 모두 src여야 한다.
        let mut files = Vec::new();
        referenced_files(&json, &mut files);
        match files.into_iter().find(|f| f != src) {
            None => Ok(()),
            Some(other) => Err(DriverError::InvalidSpec(format!(
                "image refers to another file: {other}"
            ))),
        }
    }

    async fn delete_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        pool_dir(pool)?;
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
        let path = Self::existing_path(pool, id, format)?;
        let json = self.info_json(&path, format).await?;
        let field = |k: &str| {
            json.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| DriverError::ToolFailed {
                    tool: "qemu-img",
                    stderr: format!("info output has no {k}: {json}"),
                })
        };
        Ok(VolumeInfo {
            virtual_bytes: field("virtual-size")?,
            allocated_bytes: field("actual-size")?,
        })
    }

    async fn capacity(&self, pool: &StoragePoolSpec) -> Result<Capacity, DriverError> {
        let path = pool_dir(pool)?;
        Ok(Capacity {
            total_bytes: fs4::total_space(path)?,
            available_bytes: fs4::available_space(path)?,
        })
    }

    /// 내부 스냅샷은 qcow2만 담을 수 있다.
    fn supports_snapshots(&self, format: ImageFormat) -> bool {
        format == ImageFormat::Qcow2
    }

    async fn snapshot_create(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        // qemu-img는 이름이 겹치는 스냅샷도 만든다.
        if self.tags_at(&path, format).await?.iter().any(|t| t == tag) {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        let opts = image_opts(&path, format)?;
        self.qemu_img(&["snapshot", "-q", "--image-opts", "-c", tag, &opts])
            .await?;
        Ok(())
    }

    async fn snapshot_revert(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        let opts = image_opts(&path, format)?;
        self.qemu_img(&["snapshot", "-q", "--image-opts", "-a", tag, &opts])
            .await?;
        Ok(())
    }

    async fn snapshot_delete(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        // `-d`는 태그가 없으면 실패하므로 먼저 확인한다. 겹친 태그가 있으면 모두 지운다.
        let count = self
            .tags_at(&path, format)
            .await?
            .iter()
            .filter(|t| *t == tag)
            .count();
        let opts = image_opts(&path, format)?;
        for _ in 0..count {
            self.qemu_img(&["snapshot", "-q", "--image-opts", "-d", tag, &opts])
                .await?;
        }
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let path = self.snapshot_path(pool, id, format)?;
        self.tags_at(&path, format).await
    }

    async fn resize_volume(
        &self,
        pool: &StoragePoolSpec,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let path = Self::existing_path(pool, id, format)?;
        let size = size_bytes.to_string();
        // --shrink 없이는 qemu-img가 줄이기를 거부한다.
        self.qemu_img(&[
            "resize",
            "-q",
            "-f",
            fmt_name(format),
            path_str(&path)?,
            &size,
        ])
        .await?;
        Ok(())
    }

    async fn copy_volume(
        &self,
        src_pool: &StoragePoolSpec,
        src_id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
        force_share: bool,
        dst_pool: &StoragePoolSpec,
        dst_id: Uuid,
    ) -> Result<(), DriverError> {
        let src = match tag {
            Some(_) => self.snapshot_path(src_pool, src_id, format)?,
            None => Self::existing_path(src_pool, src_id, format)?,
        };
        pool_dir(dst_pool)?;
        let dst = Self::volume_path(dst_pool, dst_id, format);
        if dst.exists() {
            return Err(DriverError::VolumeExists(dst_id));
        }
        // `-l`의 값은 옵션 문자열이므로 `,`를 `,,`로 쓴다.
        let snapshot = tag.map(|t| format!("snapshot.name={}", t.replace(',', ",,")));
        let mut before = vec!["convert", "-q", "-f", fmt_name(format)];
        if force_share {
            before.push("-U");
        }
        if let Some(s) = &snapshot {
            before.extend(["-l", s.as_str()]);
        }
        before.extend(["-O", fmt_name(format), path_str(&src)?]);
        self.write_new(dst_id, &dst, &before, &[]).await
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

    #[test]
    fn image_opts_escapes_commas() {
        assert_eq!(
            image_opts(Path::new("/pools/a,b/v.qcow2"), ImageFormat::Qcow2).unwrap(),
            "driver=qcow2,file.filename=/pools/a,,b/v.qcow2"
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
    async fn rejects_nfs_pools_for_now() {
        // qemu-img를 부르기 전에 거절한다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let p = StoragePoolSpec {
            backend: PoolBackend::Nfs {
                server: "nas".into(),
                export: "/srv/vm".into(),
                options: None,
            },
            nodes: vec![],
            shared: true,
        };
        let err = storage
            .create_volume(&p, Uuid::nil(), 1 << 20, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::InvalidSpec(_)), "{err:?}");
    }

    /// 실행 파일 셸 스크립트를 만든다. `"$last"`에 마지막 인자가 들어 있다.
    fn fake_tool(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        std::fs::write(&path, format!("#!/bin/sh\nfor last; do :; done\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn pool_files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn existing_volume_is_never_overwritten() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        let fin = DirStorage::volume_path(&p, id, ImageFormat::Raw);
        std::fs::write(&fin, b"precious").unwrap();
        let storage = DirStorage::new(fake_tool(tools.path(), "echo new > \"$last\""));

        let err = storage
            .create_volume(&p, id, 1 << 20, ImageFormat::Raw)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&fin).unwrap(), b"precious");
    }

    #[tokio::test]
    async fn volume_created_concurrently_wins_over_ours() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        let fin = DirStorage::volume_path(&p, id, ImageFormat::Qcow2);
        // 도구가 도는 사이에 다른 누군가가 같은 볼륨을 만든다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            &format!("echo other > {fin:?}\necho ours > \"$last\""),
        ));
        let src = tools.path().join("src.vmdk");
        std::fs::write(&src, b"x").unwrap();

        let err = storage
            .import_volume(&p, id, &src, SourceFormat::Vmdk, ImageFormat::Qcow2)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeExists(_)), "{err:?}");
        assert_eq!(std::fs::read(&fin).unwrap(), b"other\n");
        assert_eq!(pool_files(tmp.path()), vec![format!("{id}.qcow2")]);
    }

    #[tokio::test]
    async fn temp_file_of_an_earlier_attempt_is_not_reused() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::now_v7();
        // opld가 죽은 뒤에도 고아 qemu-img가 이전 시도의 임시 파일을 계속 쓰고 있을 수 있다.
        let orphan = tmp.path().join(format!(".{id}.raw.part"));
        std::fs::write(&orphan, b"orphan").unwrap();
        // create는 크기가 마지막 인자이므로 .part로 끝나는 출력 인자를 찾는다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            "for a; do case \"$a\" in *.part) out=\"$a\";; esac; done\necho ours > \"$out\"",
        ));

        let path = storage
            .create_volume(&p, id, 1 << 20, ImageFormat::Raw)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"ours\n");
        assert_eq!(std::fs::read(&orphan).unwrap(), b"orphan");
        assert_eq!(pool_files(tmp.path()).len(), 2);
    }

    #[tokio::test]
    async fn failed_import_leaves_no_files() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        // 출력 파일을 반쯤 쓰고 실패한다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho broken >&2\nexit 1",
        ));
        let src = tools.path().join("src.vmdk");
        std::fs::write(&src, b"x").unwrap();

        let err = storage
            .import_volume(
                &p,
                Uuid::now_v7(),
                &src,
                SourceFormat::Vmdk,
                ImageFormat::Qcow2,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert!(pool_files(tmp.path()).is_empty());

        let storage = DirStorage::new("/usr/bin/false");
        storage
            .import_volume(
                &p,
                Uuid::now_v7(),
                &src,
                SourceFormat::Vmdk,
                ImageFormat::Qcow2,
            )
            .await
            .unwrap_err();
        assert!(pool_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn relative_paths_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        // qemu-img는 상대 경로를 `json:{...}`, `nbd:` 같은 프로토콜로 해석할 수 있다.
        let storage = DirStorage::new("/usr/bin/true");
        let rel = pool("pools/local");
        let id = Uuid::nil();
        let raw = ImageFormat::Raw;
        assert!(matches!(
            storage.create_volume(&rel, id, 1 << 20, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage
                .import_volume(&rel, id, Path::new("/a.vmdk"), SourceFormat::Vmdk, raw)
                .await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.delete_volume(&rel, id, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.volume_info(&rel, id, raw).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.capacity(&rel).await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.resize_volume(&rel, id, raw, 2 << 20).await,
            Err(DriverError::InvalidSpec(_))
        ));
        let qcow2 = ImageFormat::Qcow2;
        assert!(matches!(
            storage.snapshot_create(&rel, id, qcow2, "t").await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(matches!(
            storage.snapshot_tags(&rel, id, qcow2).await,
            Err(DriverError::InvalidSpec(_))
        ));

        let abs = pool(tmp.path().to_str().unwrap());
        assert!(matches!(
            storage
                .import_volume(&abs, id, Path::new("json:{}"), SourceFormat::Vmdk, raw)
                .await,
            Err(DriverError::InvalidSpec(_))
        ));
        assert!(pool_files(tmp.path()).is_empty());
    }

    #[tokio::test]
    async fn import_source_header_is_rejected_before_running_tool() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let file = |name: &str, mut bytes: Vec<u8>| {
            bytes.resize(bytes.len().max(80), 0);
            let path = tmp.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let qcow2 = |version: u32, backing: u64, incompatible: u64| {
            let mut h = vec![0u8; 80];
            h[..4].copy_from_slice(b"QFI\xfb");
            h[4..8].copy_from_slice(&version.to_be_bytes());
            h[8..16].copy_from_slice(&backing.to_be_bytes());
            h[72..80].copy_from_slice(&incompatible.to_be_bytes());
            h
        };

        let desc = file(
            "desc.vmdk",
            b"# Disk DescriptorFile\nversion=1\nRW 2048 FLAT \"/dev/zero\" 0\n".to_vec(),
        );
        let backing = file("backing.qcow2", qcow2(3, 512, 0));
        let external = file("external.qcow2", qcow2(3, 0, 4));
        for (path, format, reason) in [
            (desc, SourceFormat::Vmdk, "descriptor"),
            (backing, SourceFormat::Qcow2, "backing file"),
            (external, SourceFormat::Qcow2, "external data file"),
        ] {
            let err = storage
                .check_import_source(&path, format)
                .await
                .unwrap_err();
            assert!(
                matches!(err, DriverError::InvalidSpec(ref m) if m.contains(reason)),
                "{reason}: {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn volume_info_without_sizes_is_tool_failure() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(DirStorage::volume_path(&p, id, ImageFormat::Raw), b"").unwrap();
        let storage = DirStorage::new(fake_tool(tools.path(), "echo '{\"virtual-size\": 5}'"));

        let err = storage
            .volume_info(&p, id, ImageFormat::Raw)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn snapshots_need_qcow2_and_existing_volume() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        let (raw, qcow2) = (ImageFormat::Raw, ImageFormat::Qcow2);
        assert!(storage.supports_snapshots(qcow2));
        assert!(!storage.supports_snapshots(raw));

        std::fs::write(DirStorage::volume_path(&p, id, raw), b"").unwrap();
        for res in [
            storage.snapshot_create(&p, id, raw, "t").await,
            storage.snapshot_revert(&p, id, raw, "t").await,
            storage.snapshot_delete(&p, id, raw, "t").await,
            storage.snapshot_tags(&p, id, raw).await.map(|_| ()),
        ] {
            assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
        }

        for res in [
            storage.snapshot_create(&p, id, qcow2, "t").await,
            storage.snapshot_revert(&p, id, qcow2, "t").await,
            storage.snapshot_delete(&p, id, qcow2, "t").await,
            storage.snapshot_tags(&p, id, qcow2).await.map(|_| ()),
            storage.resize_volume(&p, id, qcow2, 1 << 20).await,
        ] {
            assert!(
                matches!(res, Err(DriverError::VolumeNotFound(_))),
                "{res:?}"
            );
        }
    }

    #[tokio::test]
    async fn snapshot_delete_skips_missing_tag() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(DirStorage::volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // info만 답하고 다른 명령(snapshot -d)은 실패한다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            "case \"$1\" in info) echo '{\"snapshots\": [{\"name\": \"other\"}]}';; \
             *) echo unexpected >&2; exit 1;; esac",
        ));

        assert_eq!(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap(),
            vec!["other".to_string()]
        );
        storage
            .snapshot_delete(&p, id, ImageFormat::Qcow2, "gone")
            .await
            .unwrap();
        let err = storage
            .snapshot_delete(&p, id, ImageFormat::Qcow2, "other")
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn volume_without_snapshots_has_no_tags() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let id = Uuid::nil();
        std::fs::write(DirStorage::volume_path(&p, id, ImageFormat::Qcow2), b"").unwrap();
        // 스냅샷이 없으면 qemu-img info는 snapshots 항목을 내지 않는다.
        let storage = DirStorage::new(fake_tool(tools.path(), "echo '{\"virtual-size\": 5}'"));
        assert!(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn copy_volume_passes_snapshot_and_share_options() {
        let tools = tempfile::tempdir().unwrap();
        let src_dir = tempfile::tempdir().unwrap();
        // 경로와 태그의 쉼표는 옵션 문자열에서 `,,`로 쓴다.
        let dst_dir = tools.path().join("dst");
        std::fs::create_dir(&dst_dir).unwrap();
        let (src_pool, dst_pool) = (
            pool(src_dir.path().to_str().unwrap()),
            pool(dst_dir.to_str().unwrap()),
        );
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let qcow2 = ImageFormat::Qcow2;
        let src_path = DirStorage::volume_path(&src_pool, src, qcow2);
        std::fs::write(&src_path, b"").unwrap();
        // 인자를 한 줄에 하나씩 적고 출력 파일을 만든다.
        let args = tools.path().join("args");
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            &format!("printf '%s\\n' \"$@\" > {args:?}\necho copy > \"$last\""),
        ));

        storage
            .copy_volume(&src_pool, src, qcow2, Some("a,b"), true, &dst_pool, dst)
            .await
            .unwrap();
        let dst_path = DirStorage::volume_path(&dst_pool, dst, qcow2);
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"copy\n");
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-U",
                "-l",
                "snapshot.name=a,,b",
                "-O",
                "qcow2",
                src_path.to_str().unwrap(),
            ]
        );
        assert!(got[got.len() - 1].ends_with(".part"), "{got:?}");

        // 태그 없이, 잠금을 지키며 지금 내용을 복사한다.
        let dst2 = Uuid::now_v7();
        storage
            .copy_volume(&src_pool, src, qcow2, None, false, &dst_pool, dst2)
            .await
            .unwrap();
        let got = std::fs::read_to_string(&args).unwrap();
        let got: Vec<&str> = got.lines().collect();
        assert_eq!(
            got[..got.len() - 1],
            [
                "convert",
                "-q",
                "-f",
                "qcow2",
                "-O",
                "qcow2",
                src_path.to_str().unwrap()
            ]
        );
    }

    #[tokio::test]
    async fn copy_volume_checks_source_and_never_overwrites() {
        // 도구까지 가면 다른 오류(Io NotFound)가 난다.
        let storage = DirStorage::new("/nonexistent/qemu-img");
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let raw = ImageFormat::Raw;

        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeNotFound(id)) if id == src),
            "{res:?}"
        );

        std::fs::write(DirStorage::volume_path(&p, src, raw), b"").unwrap();
        // raw 볼륨에는 스냅샷이 없다.
        let res = storage
            .copy_volume(&p, src, raw, Some("t"), false, &p, dst)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");

        let dst_path = DirStorage::volume_path(&p, dst, raw);
        std::fs::write(&dst_path, b"precious").unwrap();
        let res = storage
            .copy_volume(&p, src, raw, None, false, &p, dst)
            .await;
        assert!(
            matches!(res, Err(DriverError::VolumeExists(id)) if id == dst),
            "{res:?}"
        );
        assert_eq!(std::fs::read(&dst_path).unwrap(), b"precious");

        let rel = pool("pools/local");
        let res = storage
            .copy_volume(&p, src, raw, None, false, &rel, dst)
            .await;
        assert!(matches!(res, Err(DriverError::InvalidSpec(_))), "{res:?}");
    }

    #[tokio::test]
    async fn failed_copy_leaves_no_files() {
        let tools = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let p = pool(tmp.path().to_str().unwrap());
        let src = Uuid::now_v7();
        std::fs::write(DirStorage::volume_path(&p, src, ImageFormat::Raw), b"").unwrap();
        // 출력 파일을 반쯤 쓰고 실패한다.
        let storage = DirStorage::new(fake_tool(
            tools.path(),
            "echo partial > \"$last\"\necho broken >&2\nexit 1",
        ));

        let err = storage
            .copy_volume(&p, src, ImageFormat::Raw, None, false, &p, Uuid::now_v7())
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(pool_files(tmp.path()), vec![format!("{src}.raw")]);
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
