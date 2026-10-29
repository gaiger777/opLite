//! ZFS 풀: 데이터셋 바로 아래에 볼륨마다 sparse zvol `{dataset}/opl-{id}`(raw)를 두고, 스냅샷은 ZFS
//! 스냅샷 `{dataset}/opl-{id}@{tag}`로 둔다. ZFS 명령은 [`ZfsOps`] 뒤에 있다(테스트는 가짜).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// ZFS 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const ZFS_TIMEOUT: Duration = Duration::from_secs(60);

/// 새 zvol의 장치 노드가 생기기를(udev) 기다리는 시간
const NODE_WAIT: Duration = Duration::from_secs(10);

/// udev가 이벤트를 다 다루기를 기다리는 `udevadm settle`(10초)의 명령 제한 시간
const SETTLE_TIMEOUT: Duration = Duration::from_secs(15);

/// udev를 기다리지 못했을 때 장치 노드가 머무는지 다시 보는 간격. udev가 새 zvol을 다시 다루는
/// 동안 `/dev/zvol` 링크가 잠시 사라졌다 다시 생긴다.
const NODE_STABLE: Duration = Duration::from_millis(100);

/// zvol 지우기가 "busy"로 실패하면 다시 시도하는 횟수와 그 사이 시간. 방금 닫힌 장치를
/// udev(blkid) 등이 잠시 열고 있을 수 있다.
const BUSY_RETRIES: usize = 3;
const BUSY_WAIT: Duration = Duration::from_secs(1);

/// zvol 크기는 이 배수로 올린다(volblocksize의 배수여야 한다).
const ZVOL_ALIGN: u64 = 1 << 20;

/// `zfs list`가 알려 준 zvol 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZvolInfo {
    /// 전체 이름(`{dataset}/opl-{id}`)
    pub name: String,
    /// `volsize`
    pub size_bytes: u64,
    /// `referenced`(할당된 바이트)
    pub used_bytes: u64,
}

/// zvol의 스냅샷 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsSnapshot {
    /// `@` 뒤의 이름
    pub tag: String,
    /// 스냅샷 때의 `volsize`
    pub size_bytes: u64,
}

/// 데이터셋의 `available`·`used`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZfsSpace {
    pub available_bytes: u64,
    pub used_bytes: u64,
}

/// ZFS 명령. zvol은 전체 이름이다. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait ZfsOps: Send + Sync {
    /// 데이터셋 바로 아래의 zvol들(`zfs list -t volume -d 1`)
    async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError>;
    /// zvol의 스냅샷들. 만든 순서(오래된 것부터)다.
    async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError>;
    async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError>;
    /// `size_bytes` 크기의 sparse zvol을 만든다(`zfs create -s -V`).
    async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError>;
    /// zvol을 스냅샷과 함께 지운다(`zfs destroy -r`).
    async fn destroy(&self, zvol: &str) -> Result<(), DriverError>;
    /// zvol들에 같은 이름의 스냅샷을 한 명령으로(원자적으로) 만든다.
    async fn snapshot(&self, zvols: &[&str], tag: &str) -> Result<(), DriverError>;
    /// zvol을 가장 최근 스냅샷 `tag`로 되돌린다(`zfs rollback`).
    async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError>;
    async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError>;
    async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError>;
    /// zvol 장치의 커널 버퍼를 비운다(`blockdev --flushbufs`). 되돌린 뒤 옛 데이터가 읽히지 않게.
    async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError>;
    /// zvol을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, zvol: &str) -> bool;
    /// udev가 이벤트(장치 노드 만들기·다시 다루기)를 다 다루기를 기다린다(`udevadm settle`).
    /// udev가 없으면 `false`, 시간을 넘기면 오류다.
    async fn settle(&self) -> Result<bool, DriverError>;
}

/// 실제 ZFS 명령. root여야 한다.
pub struct SystemZfs;

/// 같은 zvol을 바꾸는 앞 명령이 아직 돌고 있을 때의 일시 오류(`Timeout`)에 담는 말
const ZFS_BUSY: &str = "an earlier zfs command still running on the same zvol";

/// ZFS 명령. 바꾸는 명령의 대상은 zvol(전체 이름)이다.
static ZFS: BlockTool = BlockTool::new(
    "zfs command",
    ZFS_BUSY,
    &[("LC_ALL", "C")],
    DriverError::Zfs,
);

/// `zfs list -Hp`/`zfs get -Hp`의 탭으로 나뉜 줄들. 빈 줄은 건너뛴다.
fn rows(out: &[u8]) -> impl Iterator<Item = Vec<&str>> {
    std::str::from_utf8(out)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.split('\t').collect())
}

fn bad_output(what: impl std::fmt::Display) -> DriverError {
    DriverError::Zfs(format!("unparseable zfs output: {what}"))
}

fn number(v: &str) -> Result<u64, DriverError> {
    v.trim()
        .parse()
        .map_err(|_| bad_output(format!("number {v:?}")))
}

/// `zfs list -Hp -o name,volsize,referenced`의 출력
fn parse_zvols(out: &[u8]) -> Result<Vec<ZvolInfo>, DriverError> {
    rows(out)
        .map(|row| match row.as_slice() {
            [name, size, used] => Ok(ZvolInfo {
                name: name.to_string(),
                size_bytes: number(size)?,
                used_bytes: number(used)?,
            }),
            _ => Err(bad_output(format!("zvol row {row:?}"))),
        })
        .collect()
}

/// `zfs list -Hp -t snapshot -o name,volsize`의 출력
fn parse_snapshots(out: &[u8]) -> Result<Vec<ZfsSnapshot>, DriverError> {
    rows(out)
        .map(|row| match row.as_slice() {
            [name, size] => {
                let (_, tag) = name
                    .split_once('@')
                    .ok_or_else(|| bad_output(format!("snapshot name {name:?}")))?;
                Ok(ZfsSnapshot {
                    tag: tag.to_string(),
                    size_bytes: number(size)?,
                })
            }
            _ => Err(bad_output(format!("snapshot row {row:?}"))),
        })
        .collect()
}

/// `zfs get -Hp -o value available,used`의 출력
fn parse_space(out: &[u8]) -> Result<ZfsSpace, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [available, used] if available.len() == 1 && used.len() == 1 => Ok(ZfsSpace {
            available_bytes: number(available[0])?,
            used_bytes: number(used[0])?,
        }),
        _ => Err(bad_output(format!("space {values:?}"))),
    }
}

#[async_trait]
impl ZfsOps for SystemZfs {
    async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError> {
        let args = [
            "list",
            "-Hp",
            "-t",
            "volume",
            "-d",
            "1",
            "-o",
            "name,volsize,referenced",
            dataset,
        ];
        parse_zvols(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError> {
        let args = [
            "list",
            "-Hp",
            "-t",
            "snapshot",
            "-d",
            "1",
            "-s",
            "createtxg",
            "-o",
            "name,volsize",
            zvol,
        ];
        parse_snapshots(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError> {
        let args = ["get", "-Hp", "-o", "value", "available,used", dataset];
        parse_space(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
        let size = size_bytes.to_string();
        ZFS.run_change("zfs", &create_args(zvol, &size), &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
        ZFS.run_change("zfs", &["destroy", "-r", zvol], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn snapshot(&self, zvols: &[&str], tag: &str) -> Result<(), DriverError> {
        let names: Vec<String> = zvols.iter().map(|z| format!("{z}@{tag}")).collect();
        let mut args = vec!["snapshot"];
        args.extend(names.iter().map(String::as_str));
        ZFS.run_change("zfs", &args, zvols, ZFS_TIMEOUT).await
    }

    async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
        let name = format!("{zvol}@{tag}");
        ZFS.run_change("zfs", &["rollback", &name], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
        let name = format!("{zvol}@{tag}");
        ZFS.run_change("zfs", &["destroy", &name], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
        let prop = format!("volsize={size_bytes}");
        ZFS.run_change("zfs", &["set", &prop, zvol], &[zvol], ZFS_TIMEOUT)
            .await
    }

    async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError> {
        let dev = device_path(Path::new(super::lvm::DEV), zvol, None);
        let args = ["--flushbufs", path_str(&dev)?];
        ZFS.run_change("blockdev", &args, &[zvol], ZFS_TIMEOUT)
            .await
    }

    fn changing(&self, zvol: &str) -> bool {
        ZFS.changing(zvol)
    }

    async fn settle(&self) -> Result<bool, DriverError> {
        let args = ["settle", "--timeout=10"];
        match ZFS.run_read("udevadm", &args, SETTLE_TIMEOUT).await {
            Ok(_) => Ok(true),
            Err(DriverError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// sparse zvol을 만드는 `zfs` 인자. 장치로만 보이고(`volmode=dev`) 스냅샷 장치도 보인다
/// (`snapdev=visible`). 이 속성을 따르는 자동 스냅샷 도구(zfs-auto-snapshot 등)가 찍지 않게
/// `com.sun:auto-snapshot=false`를 둔다: 더 최근 스냅샷이 생기면 되돌리기가 막힌다.
fn create_args<'a>(zvol: &'a str, size: &'a str) -> [&'a str; 11] {
    [
        "create",
        "-s",
        "-V",
        size,
        "-o",
        "volmode=dev",
        "-o",
        "snapdev=visible",
        "-o",
        "com.sun:auto-snapshot=false",
        zvol,
    ]
}

/// 볼륨의 zvol 이름
fn volume_zvol(dataset: &str, id: Uuid) -> String {
    format!("{dataset}/opl-{id}")
}

/// zvol(`snapshot`이 있으면 그 스냅샷)의 장치 노드 `{dev}/zvol/{zvol}[@{snapshot}]`
fn device_path(dev: &Path, zvol: &str, snapshot: Option<&str>) -> PathBuf {
    let name = match snapshot {
        Some(tag) => format!("{zvol}@{tag}"),
        None => zvol.to_string(),
    };
    dev.join("zvol").join(name)
}

/// 있는 ZFS 볼륨의 장치 노드. 없으면 `VolumeNotActive`(노드가 아직 생기지 않았다).
pub(super) fn volume_device(dev: &Path, dataset: &str, id: Uuid) -> Result<PathBuf, DriverError> {
    let path = device_path(dev, &volume_zvol(dataset, id), None);
    if path.exists() {
        Ok(path)
    } else {
        Err(DriverError::VolumeNotActive(path))
    }
}

/// ZFS 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "zfs pools hold raw volumes".into(),
        )),
    }
}

/// zvol 크기: `ZVOL_ALIGN` 배수로 올린다.
fn zvol_size(size_bytes: u64) -> Result<u64, DriverError> {
    size_bytes
        .div_ceil(ZVOL_ALIGN)
        .checked_mul(ZVOL_ALIGN)
        .ok_or_else(|| DriverError::InvalidSpec(format!("volume size {size_bytes} is too large")))
}

/// ZFS 풀(`zfs`)의 볼륨. 볼륨은 sparse zvol이고 스냅샷은 ZFS 스냅샷이다.
pub struct ZfsStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn ZfsOps>,
    /// 장치 노드가 놓이는 디렉터리(`/dev`, zvol은 `{dev}/zvol/...`). 테스트는 바꾼다.
    dev: PathBuf,
    /// 장치 노드가 생기기를 기다리는 시간([`NODE_WAIT`]). 테스트는 줄인다.
    node_wait: Duration,
    /// busy인 zvol 지우기를 다시 시도하기 전에 기다리는 시간([`BUSY_WAIT`]). 테스트는 줄인다.
    busy_wait: Duration,
}

impl ZfsStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn ZfsOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(super::lvm::DEV),
            node_wait: NODE_WAIT,
            busy_wait: BUSY_WAIT,
        }
    }

    /// 풀의 데이터셋. raw가 아니면 `InvalidSpec`.
    fn dataset(pool: &StoragePool, format: ImageFormat) -> Result<&str, DriverError> {
        require_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::Zfs { dataset } => Ok(dataset),
            PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } | PoolBackend::LvmThin { .. } => {
                Err(DriverError::InvalidSpec("not a zfs pool".into()))
            }
        }
    }

    /// 스냅샷 태그는 ZFS 이름에 쓸 수 있는 글자여야 한다.
    fn check_tag(tag: &str) -> Result<(), DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c));
        if ok {
            Ok(())
        } else {
            Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid zfs name"
            )))
        }
    }

    /// 데이터셋 바로 아래의 zvol `zvol`
    async fn find(&self, dataset: &str, zvol: &str) -> Result<Option<ZvolInfo>, DriverError> {
        Ok(self
            .ops
            .volumes(dataset)
            .await?
            .into_iter()
            .find(|v| v.name == zvol))
    }

    /// 있는 볼륨의 zvol. 없으면 `VolumeNotFound`.
    async fn existing(&self, dataset: &str, id: Uuid) -> Result<ZvolInfo, DriverError> {
        self.find(dataset, &volume_zvol(dataset, id))
            .await?
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 있는 볼륨의 스냅샷들(만든 순서)
    async fn existing_snapshots(
        &self,
        dataset: &str,
        id: Uuid,
    ) -> Result<Vec<ZfsSnapshot>, DriverError> {
        let zvol = self.existing(dataset, id).await?.name;
        self.ops.snapshots(&zvol).await
    }

    /// 볼륨이 있고 스냅샷 `tag`도 있어야 한다. 볼륨의 스냅샷들(만든 순서)과 그 스냅샷을 돌려준다.
    async fn existing_snapshot(
        &self,
        dataset: &str,
        id: Uuid,
        tag: &str,
    ) -> Result<(Vec<ZfsSnapshot>, ZfsSnapshot), DriverError> {
        let snapshots = self.existing_snapshots(dataset, id).await?;
        let found = snapshots
            .iter()
            .find(|s| s.tag == tag)
            .cloned()
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
            })?;
        Ok((snapshots, found))
    }

    /// zvol을 스냅샷과 함께 지운다. "busy"로 실패하면 [`BUSY_RETRIES`]번까지 다시 시도한다.
    async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
        let mut retries = 0;
        loop {
            match self.ops.destroy(zvol).await {
                Err(DriverError::Zfs(msg)) if msg.contains("busy") && retries < BUSY_RETRIES => {
                    tracing::debug!(zvol, error = %msg, "zvol busy, retrying destroy");
                    retries += 1;
                    tokio::time::sleep(self.busy_wait).await;
                }
                result => return result,
            }
        }
    }

    /// udev가 이벤트를 다 다루기를 기다린다. 기다리지 못했으면(udev가 없거나 시간을 넘김) `false`.
    async fn settle(&self) -> bool {
        match self.ops.settle().await {
            Ok(settled) => settled,
            Err(e) => {
                tracing::warn!(error = %e, "udev did not settle");
                false
            }
        }
    }

    /// udev를 기다린 뒤 장치 노드 `path`가 생기기를 기다린다. udev를 기다리지 못했으면 노드가 잠시
    /// 사라졌다 다시 생길 수 있으므로 [`NODE_STABLE`] 떨어진 두 번의 확인에서 모두 보여야 한다.
    /// 끝내 없으면 `VolumeNotActive`.
    async fn wait_node(&self, path: PathBuf) -> Result<PathBuf, DriverError> {
        let settled = self.settle().await;
        let deadline = tokio::time::Instant::now() + self.node_wait;
        let mut seen = false;
        loop {
            if path.exists() {
                if settled || seen {
                    return Ok(path);
                }
                seen = true;
            } else {
                seen = false;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(DriverError::VolumeNotActive(path));
            }
            let pause = if seen {
                NODE_STABLE
            } else {
                Duration::from_millis(50)
            };
            tokio::time::sleep(pause).await;
        }
    }

    /// 새 볼륨 zvol을 만들고 장치 노드를 돌려준다. 크기는 1 MiB 배수로 올린다. 장치 노드가 생기지
    /// 않으면 만든 zvol을 지운다.
    async fn create_zvol(
        &self,
        dataset: &str,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<PathBuf, DriverError> {
        let zvol = volume_zvol(dataset, id);
        if self.find(dataset, &zvol).await?.is_some() {
            return Err(DriverError::VolumeExists(id));
        }
        self.ops.create(&zvol, zvol_size(size_bytes)?).await?;
        match self.wait_node(device_path(&self.dev, &zvol, None)).await {
            Ok(path) => Ok(path),
            Err(e) => {
                if let Err(del) = self.destroy(&zvol).await {
                    tracing::warn!(volume = %id, error = %del, "cannot remove volume without device");
                }
                Err(e)
            }
        }
    }

    /// 만든 zvol(장치 `dst`)에 `qemu-img convert -n`으로 써 넣는다. 새 sparse zvol은 0으로 읽히므로
    /// 0인 부분은 쓰지 않는다(`--target-is-zero`). 실패하면 zvol을 지운다.
    async fn fill(
        &self,
        dataset: &str,
        id: Uuid,
        dst: &Path,
        from: &[String],
    ) -> Result<(), DriverError> {
        let written = async {
            let mut args = vec!["convert", "-q", "-n", "--target-is-zero"];
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(dst)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.destroy(&volume_zvol(dataset, id)).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for ZfsStorage {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        self.create_zvol(dataset, id, size_bytes).await
    }

    /// 원본의 가상 크기로 zvol을 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let dst = self.create_zvol(dataset, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(dataset, id, &dst, &from).await?;
        Ok(dst)
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// zvol을 스냅샷과 함께 지운다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?.name;
        self.destroy(&zvol).await
    }

    /// zvol은 늘 활성이다. 볼륨이 있고 장치 노드가 있는지만 본다.
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        self.existing(dataset, id).await?;
        volume_device(&self.dev, dataset, id).map(drop)
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: zvol.size_bytes,
            allocated_bytes: zvol.used_bytes,
        })
    }

    /// 데이터셋의 크기(`available`+`used`)와 남은 공간(`available`)
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let dataset = Self::dataset(pool, ImageFormat::Raw)?;
        let space = self.ops.space(dataset).await?;
        Ok(Capacity {
            total_bytes: space.available_bytes.saturating_add(space.used_bytes),
            available_bytes: space.available_bytes,
        })
    }

    /// 볼륨이 raw이므로 ZFS 스냅샷으로 스냅샷을 둔다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Raw
    }

    /// VM이 실행 중이어도 만들 수 있다(일관성은 호출자가 VM을 잠시 멈춰 맞춘다).
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        self.snapshot_many(pool, &[id], format, tag).await
    }

    /// 모든 볼륨이 있고 어느 볼륨에도 `tag`가 없으면 한 명령으로(원자적으로) 찍는다.
    async fn snapshot_many(
        &self,
        pool: &StoragePool,
        ids: &[Uuid],
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        Self::check_tag(tag)?;
        if ids.is_empty() {
            return Ok(());
        }
        let volumes = self.ops.volumes(dataset).await?;
        let mut zvols = Vec::with_capacity(ids.len());
        for &id in ids {
            let zvol = volume_zvol(dataset, id);
            if !volumes.iter().any(|v| v.name == zvol) {
                return Err(DriverError::VolumeNotFound(id));
            }
            if self
                .ops
                .snapshots(&zvol)
                .await?
                .iter()
                .any(|s| s.tag == tag)
            {
                return Err(DriverError::InvalidSpec(format!(
                    "snapshot {tag} already exists on volume {id}"
                )));
            }
            zvols.push(zvol);
        }
        let zvols: Vec<&str> = zvols.iter().map(String::as_str).collect();
        self.ops.snapshot(&zvols, tag).await?;
        // 스냅샷 장치 노드가 생긴다(`snapdev=visible`).
        self.settle().await;
        Ok(())
    }

    /// 그 볼륨의 가장 최근 스냅샷으로만 되돌린다(`zfs rollback`은 더 최근 스냅샷이 있으면 그것을
    /// 지워야 한다). 되돌리기 앞뒤로 장치의 커널 버퍼를 비운다(앞: 아직 쓰이지 않은 버퍼가 되돌린
    /// 뒤에 쓰이지 않게, 뒤: 남은 페이지 캐시가 옛 데이터를 보이지 않게). 비우지 못하면 오류고, 앞에서
    /// 못 비우면 되돌리지 않는다. 되돌린 뒤 udev를 기다리고 장치 노드가 있는지 본다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let (snapshots, _) = self.existing_snapshot(dataset, id, tag).await?;
        if snapshots.last().is_some_and(|s| s.tag != tag) {
            return Err(DriverError::InvalidSpec(
                "zfs can only revert to the latest snapshot of a volume".into(),
            ));
        }
        let zvol = volume_zvol(dataset, id);
        self.ops.flush_buffers(&zvol).await?;
        self.ops.rollback(&zvol, tag).await?;
        // udev가 되돌린 장치를 다시 다루는 동안 장치 노드가 잠시 사라질 수 있다.
        self.wait_node(device_path(&self.dev, &zvol, None)).await?;
        self.ops.flush_buffers(&zvol).await
    }

    /// VM이 실행 중이어도 지울 수 있다. 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 zvol을
    /// 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 곧 생길 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        Self::check_tag(tag)?;
        let zvol = volume_zvol(dataset, id);
        if self.find(dataset, &zvol).await?.is_none() {
            return Ok(());
        }
        if self.ops.changing(&zvol) {
            return Err(DriverError::Timeout(ZFS_BUSY));
        }
        if self
            .ops
            .snapshots(&zvol)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            self.ops.destroy_snapshot(&zvol, tag).await?;
        }
        Ok(())
    }

    /// 만든 순서다.
    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let snapshots = self.existing_snapshots(dataset, id).await?;
        Ok(snapshots.into_iter().map(|s| s.tag).collect())
    }

    /// VM이 실행 중이어도 늘릴 수 있다(그 뒤 QEMU에 새 크기를 알리는 것은 호출자의 일이다). 크기는
    /// 1 MiB 배수로 올린다. 이미 그 크기 이상이면(앞선 시도가 늘렸다) 아무것도 하지 않는다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = self.existing(dataset, id).await?;
        if zvol.size_bytes >= size_bytes {
            return Ok(());
        }
        self.ops
            .set_volsize(&zvol.name, zvol_size(size_bytes)?)
            .await
    }

    /// zvol 장치, 또는 스냅샷의 장치(`snapdev=visible`). 내놓을 것은 없다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let (zvol, size_bytes) = match tag {
            Some(tag) => {
                let (_, snapshot) = self.existing_snapshot(dataset, id, tag).await?;
                (volume_zvol(dataset, id), snapshot.size_bytes)
            }
            None => {
                let zvol = self.existing(dataset, id).await?;
                (zvol.name, zvol.size_bytes)
            }
        };
        let path = self.wait_node(device_path(&self.dev, &zvol, tag)).await?;
        Ok(VolumeSource {
            path,
            format,
            snapshot: None,
            size_bytes,
        })
    }

    async fn release_source(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: Option<&str>,
    ) -> Result<(), DriverError> {
        Ok(())
    }

    /// 원본(스냅샷이면 그때)의 크기로 대상 zvol을 만들고 원본을 써 넣는다. 데이터셋이 달라도 같다.
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
        let dataset = Self::dataset(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        let dst = self.create_zvol(dataset, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, dst_format)?;
        self.fill(dataset, dst_id, &dst, &from).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    struct FakeZvol {
        size: u64,
        /// 만든 순서의 스냅샷(태그, 크기)
        snapshots: Vec<(String, u64)>,
    }

    /// 데이터셋 `tank/opl`, `tank/other`를 메모리에 두는 ZFS. 바꾸는 호출을 적고, `fail`에 적은 호출은
    /// 실패한다. `busy`에 적은 호출은 남은 횟수만큼 "busy"로 실패한다. zvol과 스냅샷의 장치 노드를
    /// `dev/zvol` 아래에 둔다(udev처럼). `no_udev`면 udev 기다리기를 하지 못한다.
    struct FakeZfs {
        zvols: Mutex<BTreeMap<String, FakeZvol>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        busy: Mutex<BTreeMap<String, usize>>,
        /// 바꾸는 명령이 아직 돌고 있다고 알리는 zvol
        changing: Mutex<Vec<String>>,
        no_udev: Mutex<bool>,
        dev: tempfile::TempDir,
    }

    impl Default for FakeZfs {
        fn default() -> Self {
            Self {
                zvols: Mutex::default(),
                log: Mutex::default(),
                fail: Mutex::default(),
                busy: Mutex::default(),
                changing: Mutex::default(),
                no_udev: Mutex::default(),
                dev: tempfile::tempdir().unwrap(),
            }
        }
    }

    impl FakeZfs {
        /// zvol(`snapshot`이 있으면 그 스냅샷)의 장치 노드 경로
        fn node(&self, zvol: &str, snapshot: Option<&str>) -> PathBuf {
            device_path(self.dev.path(), zvol, snapshot)
        }

        /// 장치 노드를 zvol과 스냅샷에 맞춘다.
        fn sync_nodes(&self, zvols: &BTreeMap<String, FakeZvol>) {
            let root = self.dev.path().join("zvol");
            let _ = std::fs::remove_dir_all(&root);
            for (name, zvol) in zvols {
                let node = self.node(name, None);
                std::fs::create_dir_all(node.parent().unwrap()).unwrap();
                std::fs::write(&node, b"").unwrap();
                for (tag, _) in &zvol.snapshots {
                    std::fs::write(self.node(name, Some(tag)), b"").unwrap();
                }
            }
        }

        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Zfs(format!("injected: {call}")));
            }
            if let Some(left) = self.busy.lock().unwrap().get_mut(&call)
                && *left > 0
            {
                *left -= 1;
                return Err(DriverError::Zfs(format!("{call}: dataset is busy")));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        /// 호출 `call`이 `times`번 "busy"로 실패한다.
        fn busy_on(&self, call: impl Into<String>, times: usize) {
            self.busy.lock().unwrap().insert(call.into(), times);
        }

        /// 적은 호출(udev 기다리기 `settle`까지)
        fn taken_all(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        /// 적은 호출 중 udev 기다리기를 뺀 것
        fn taken(&self) -> Vec<String> {
            let mut log = self.taken_all();
            log.retain(|c| c != "settle");
            log
        }

        fn names(&self) -> Vec<String> {
            self.zvols.lock().unwrap().keys().cloned().collect()
        }

        fn zvol(&self, name: &str) -> Option<FakeZvol> {
            self.zvols.lock().unwrap().get(name).cloned()
        }

        fn missing(name: &str) -> DriverError {
            DriverError::Zfs(format!("cannot open '{name}': dataset does not exist"))
        }

        /// 데이터셋 `dataset` 바로 아래의 이름인지
        fn in_dataset(dataset: &str, name: &str) -> bool {
            name.strip_prefix(dataset)
                .and_then(|rest| rest.strip_prefix('/'))
                .is_some_and(|rest| !rest.contains('/'))
        }
    }

    #[async_trait]
    impl ZfsOps for FakeZfs {
        async fn volumes(&self, dataset: &str) -> Result<Vec<ZvolInfo>, DriverError> {
            if !["tank/opl", "tank/other"].contains(&dataset) {
                return Err(Self::missing(dataset));
            }
            Ok(self
                .zvols
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| Self::in_dataset(dataset, name))
                .map(|(name, z)| ZvolInfo {
                    name: name.clone(),
                    size_bytes: z.size,
                    used_bytes: z.size / 4,
                })
                .collect())
        }

        async fn snapshots(&self, zvol: &str) -> Result<Vec<ZfsSnapshot>, DriverError> {
            let zvols = self.zvols.lock().unwrap();
            let z = zvols.get(zvol).ok_or_else(|| Self::missing(zvol))?;
            Ok(z.snapshots
                .iter()
                .map(|(tag, size)| ZfsSnapshot {
                    tag: tag.clone(),
                    size_bytes: *size,
                })
                .collect())
        }

        async fn space(&self, dataset: &str) -> Result<ZfsSpace, DriverError> {
            if dataset != "tank/opl" {
                return Err(Self::missing(dataset));
            }
            Ok(ZfsSpace {
                available_bytes: 6 << 30,
                used_bytes: 2 << 30,
            })
        }

        async fn create(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("create {zvol} {size_bytes}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            if zvols.contains_key(zvol) {
                return Err(DriverError::Zfs(format!("cannot create '{zvol}': exists")));
            }
            let z = FakeZvol {
                size: size_bytes,
                snapshots: vec![],
            };
            zvols.insert(zvol.into(), z);
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("destroy -r {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let removed = zvols.remove(zvol);
            self.sync_nodes(&zvols);
            removed.map(drop).ok_or_else(|| Self::missing(zvol))
        }

        async fn snapshot(&self, names: &[&str], tag: &str) -> Result<(), DriverError> {
            self.call(format!("snapshot {} @{tag}", names.join(" ")))?;
            let mut zvols = self.zvols.lock().unwrap();
            // 한 명령은 모두 찍거나 하나도 찍지 않는다.
            for name in names {
                let z = zvols.get(*name).ok_or_else(|| Self::missing(name))?;
                if z.snapshots.iter().any(|(t, _)| t == tag) {
                    return Err(DriverError::Zfs(format!("{name}@{tag} exists")));
                }
            }
            for name in names {
                let z = zvols.get_mut(*name).unwrap();
                z.snapshots.push((tag.into(), z.size));
            }
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn rollback(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
            self.call(format!("rollback {zvol}@{tag}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            match z.snapshots.last() {
                Some((t, size)) if t == tag => z.size = *size,
                _ => return Err(DriverError::Zfs("more recent snapshots exist".into())),
            }
            Ok(())
        }

        async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
            self.call(format!("destroy {zvol}@{tag}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            let before = z.snapshots.len();
            z.snapshots.retain(|(t, _)| t != tag);
            if z.snapshots.len() == before {
                return Err(Self::missing(&format!("{zvol}@{tag}")));
            }
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn set_volsize(&self, zvol: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("set volsize={size_bytes} {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let z = zvols.get_mut(zvol).ok_or_else(|| Self::missing(zvol))?;
            if size_bytes < z.size {
                return Err(DriverError::Zfs("cannot shrink".into()));
            }
            z.size = size_bytes;
            Ok(())
        }

        async fn flush_buffers(&self, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("flushbufs {zvol}"))
        }

        fn changing(&self, zvol: &str) -> bool {
            self.changing.lock().unwrap().iter().any(|z| z == zvol)
        }

        async fn settle(&self) -> Result<bool, DriverError> {
            self.call("settle".into())?;
            Ok(!*self.no_udev.lock().unwrap())
        }
    }

    fn zfs_pool(dataset: &str) -> StoragePool {
        Resource::new(
            "z",
            StoragePoolSpec {
                backend: PoolBackend::Zfs {
                    dataset: dataset.into(),
                },
                nodes: vec![],
                shared: false,
            },
        )
    }

    fn pool() -> StoragePool {
        zfs_pool("tank/opl")
    }

    fn zvol(id: Uuid) -> String {
        format!("tank/opl/opl-{id}")
    }

    /// `info`면 가상 크기 1000을 알리고, 그 밖은 인자를 한 줄에 하나씩 `{dir}/args`에 적는 가짜
    /// qemu-img. `fail`이면 `info`가 아닌 호출이 실패한다.
    fn fake_qemu_img(dir: &Path, fail: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = info ]; then echo '{{\"virtual-size\": 1000}}'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {args:?}\n{end}\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeZfs>, ZfsStorage) {
        let zfs = Arc::new(FakeZfs::default());
        let storage = ZfsStorage {
            dev: zfs.dev.path().to_path_buf(),
            busy_wait: Duration::from_millis(10),
            ..ZfsStorage::new(qemu_img, zfs.clone())
        };
        (zfs, storage)
    }

    fn invalid(e: DriverError, want: &str) {
        match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains(want), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn not_found(e: DriverError, id: Uuid) {
        assert!(
            matches!(e, DriverError::VolumeNotFound(v) if v == id),
            "{e:?}"
        );
    }

    const RAW: ImageFormat = ImageFormat::Raw;
    const MIB: u64 = 1 << 20;

    /// 크기를 1 MiB 배수로 올린 sparse zvol을 만들고 장치 노드를 돌려준다.
    #[tokio::test]
    async fn creates_raw_sparse_zvols() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let path = storage.create_volume(&p, id, MIB + 1, RAW).await.unwrap();
        assert_eq!(path, zfs.node(&zvol(id), None));
        assert_eq!(path, zfs.dev.path().join(format!("zvol/tank/opl/opl-{id}")));
        // 만든 뒤 udev가 장치 노드를 다 다룰 때까지 기다린다.
        assert_eq!(
            zfs.taken_all(),
            [format!("create {} {}", zvol(id), 2 * MIB), "settle".into()]
        );

        let err = storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), MIB, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), u64::MAX, RAW)
                .await
                .unwrap_err(),
            "too large",
        );
        assert_eq!(zfs.taken(), Vec::<String>::new());
        // 데이터셋이 없으면 ZFS 오류다.
        let err = storage
            .create_volume(&zfs_pool("tank/none"), id, MIB, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
    }

    /// udev를 기다리지 못하면(udev가 없거나 시간을 넘김) 장치 노드가 잠시 사라졌다 다시 생길 수
    /// 있으므로 떨어진 두 번의 확인에서 모두 보여야 한다.
    #[tokio::test]
    async fn without_udev_settle_the_device_node_must_stay() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        *zfs.no_udev.lock().unwrap() = true;
        let node = zfs.dev.path().join("node");
        std::fs::write(&node, b"").unwrap();
        let flicker = {
            let node = node.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                std::fs::remove_file(&node).unwrap();
                tokio::time::sleep(Duration::from_millis(300)).await;
                std::fs::write(&node, b"").unwrap();
            })
        };
        let started = std::time::Instant::now();
        assert_eq!(storage.wait_node(node.clone()).await.unwrap(), node);
        assert!(started.elapsed() >= Duration::from_millis(330));
        flicker.await.unwrap();
        assert_eq!(zfs.taken_all(), ["settle"]);
        // udev를 기다렸으면 바로 돌려준다.
        *zfs.no_udev.lock().unwrap() = false;
        let started = std::time::Instant::now();
        storage.wait_node(node.clone()).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        // 기다리기가 실패해도(시간을 넘김) 기록만 하고 노드가 머무는지 본다.
        zfs.fail_on("settle");
        let started = std::time::Instant::now();
        storage.wait_node(node.clone()).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(100));
    }

    /// 장치 노드가 생기지 않으면 만든 zvol을 지운다.
    #[tokio::test]
    async fn create_without_device_node_destroys_the_zvol() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let storage = ZfsStorage {
            dev: zfs.dev.path().join("elsewhere"),
            node_wait: Duration::from_millis(100),
            ..storage
        };
        let (p, id) = (pool(), Uuid::now_v7());
        let err = storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(id)),
                format!("destroy -r {}", zvol(id)),
            ]
        );
        assert!(zfs.names().is_empty());
    }

    /// 원본의 가상 크기로 zvol을 만들고 raw로 변환해 써 넣는다(새 zvol은 0으로 읽힌다). 실패하면
    /// zvol을 지운다.
    #[tokio::test]
    async fn imports_into_a_new_zvol() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, zfs.node(&zvol(id), None));
        assert_eq!(zfs.taken(), [format!("create {} {MIB}", zvol(id))]);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "vmdk",
                "-O",
                "raw",
                "/up/disk.vmdk",
                path.to_str().unwrap(),
            ]
        );

        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(id)),
                format!("destroy -r {}", zvol(id)),
            ]
        );
        assert!(zfs.names().is_empty());
        invalid(
            storage
                .import_volume(&p, id, "x.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );
    }

    /// 지우기는 zvol을 스냅샷과 함께 지운다. 없으면 `VolumeNotFound`다.
    #[tokio::test]
    async fn delete_destroys_zvol_with_its_snapshots() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [id, other] {
            storage.create_volume(&p, v, MIB, RAW).await.unwrap();
            storage.snapshot_create(&p, v, RAW, "a").await.unwrap();
        }
        zfs.taken();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(zfs.taken(), [format!("destroy -r {}", zvol(id))]);
        assert_eq!(zfs.names(), [zvol(other)]);
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);
        assert!(zfs.taken().is_empty());
    }

    /// zvol 지우기가 "busy"로 실패하면(방금 닫힌 장치를 아직 누가 열고 있다) 세 번까지 다시
    /// 시도한다. 지우기, 만들기 직후 정리, 가져오기 실패 정리가 같다.
    #[tokio::test]
    async fn destroy_retries_while_busy() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        let (p, id) = (pool(), Uuid::now_v7());
        let destroy = format!("destroy -r {}", zvol(id));
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 3);
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(zfs.taken(), vec![destroy.clone(); 4]);
        assert!(zfs.names().is_empty());

        // 네 번째도 busy면 그 오류다.
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 4);
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("busy")),
            "{err:?}"
        );
        assert_eq!(zfs.taken(), vec![destroy.clone(); 4]);
        assert_eq!(zfs.names(), [zvol(id)]);
        // busy가 아닌 실패는 다시 시도하지 않는다.
        zfs.fail_on(&destroy);
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert_eq!(zfs.taken(), [destroy.as_str()]);

        // 가져오기에 실패하면 busy여도 만든 zvol을 치운다.
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        zfs.busy_on(&destroy, 2);
        storage
            .import_volume(&p, id, "/up/disk.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert_eq!(zfs.taken()[1..], vec![destroy.clone(); 3]);
        assert!(zfs.names().is_empty());

        // 장치 노드가 생기지 않아 지울 때도 같다.
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let storage = ZfsStorage {
            dev: zfs.dev.path().join("elsewhere"),
            node_wait: Duration::from_millis(100),
            ..storage
        };
        zfs.busy_on(&destroy, 1);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap_err();
        assert_eq!(zfs.taken()[1..], vec![destroy.clone(); 2]);
        assert!(zfs.names().is_empty());
    }

    #[tokio::test]
    async fn reports_volume_usage_and_dataset_capacity() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.volume_info(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, 4 * MIB, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: MIB,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        let err = storage.capacity(&zfs_pool("tank/none")).await.unwrap_err();
        assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        // 다른 데이터셋의 zvol은 이 풀의 볼륨이 아니다.
        let other = zfs_pool("tank/other");
        not_found(storage.volume_info(&other, id, RAW).await.unwrap_err(), id);
        assert_eq!(zfs.taken().len(), 1);
    }

    /// zvol은 늘 활성이다. 볼륨과 장치 노드가 있는지만 본다.
    #[tokio::test]
    async fn activate_checks_volume_and_device_node() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.activate_volume(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert!(zfs.taken().is_empty());
        std::fs::remove_file(zfs.node(&zvol(id), None)).unwrap();
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeNotActive(ref p) if *p == zfs.node(&zvol(id), None)),
            "{err:?}"
        );
        assert!(err.is_transient());
    }

    /// 크기를 1 MiB 배수로 올려 늘린다. 이미 그 크기 이상이면 아무것도 하지 않는다.
    #[tokio::test]
    async fn resize_sets_volsize() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage
                .resize_volume(&p, id, RAW, 2 * MIB)
                .await
                .unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        storage
            .resize_volume(&p, id, RAW, 2 * MIB + 1)
            .await
            .unwrap();
        assert_eq!(
            zfs.taken(),
            [format!("set volsize={} {}", 3 * MIB, zvol(id))]
        );
        storage.resize_volume(&p, id, RAW, 3 * MIB).await.unwrap();
        storage.resize_volume(&p, id, RAW, MIB).await.unwrap();
        assert!(zfs.taken().is_empty());
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, 3 * MIB);
    }

    #[tokio::test]
    async fn snapshots_are_zfs_snapshots() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
        not_found(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        not_found(storage.snapshot_tags(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();

        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {} @b", zvol(id)),
                format!("snapshot {} @a", zvol(id)),
            ]
        );
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        for bad in ["a/b", "a@b", "", "a b"] {
            invalid(
                storage.snapshot_create(&p, id, RAW, bad).await.unwrap_err(),
                "not a valid zfs name",
            );
        }
        // 만든 순서다.
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["b", "a"]
        );

        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        // 없는 태그나 볼륨은 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        storage
            .snapshot_delete(&p, Uuid::now_v7(), RAW, "b")
            .await
            .unwrap();
        assert_eq!(zfs.taken(), [format!("destroy {}@b", zvol(id))]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["a"]);
        invalid(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
    }

    /// 그 zvol을 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 아직 없어도 지우기는
    /// 일시 오류다. 명령이 끝나면 태그가 생길 수 있다.
    #[tokio::test]
    async fn snapshot_delete_waits_for_a_running_command() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.taken();
        zfs.changing.lock().unwrap().push(zvol(id));
        let err = storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        zfs.changing.lock().unwrap().clear();
        storage.snapshot_delete(&p, id, RAW, "t").await.unwrap();
        assert!(zfs.taken().is_empty());
    }

    /// 여러 볼륨을 한 명령으로 찍는다. 볼륨이 없거나 태그가 이미 있으면 아무것도 찍지 않는다.
    #[tokio::test]
    async fn snapshot_many_takes_one_atomic_command() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (a, b, missing) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        for v in [a, b] {
            storage.create_volume(&p, v, MIB, RAW).await.unwrap();
        }
        zfs.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        // 스냅샷 장치 노드가 생기므로 udev를 기다린다.
        assert_eq!(
            zfs.taken_all(),
            [
                format!("snapshot {} {} @t", zvol(a), zvol(b)),
                "settle".into()
            ]
        );
        for v in [a, b] {
            assert_eq!(storage.snapshot_tags(&p, v, RAW).await.unwrap(), ["t"]);
        }

        not_found(
            storage
                .snapshot_many(&p, &[a, missing], RAW, "u")
                .await
                .unwrap_err(),
            missing,
        );
        storage.snapshot_create(&p, b, RAW, "u").await.unwrap();
        zfs.taken();
        invalid(
            storage
                .snapshot_many(&p, &[a, b], RAW, "u")
                .await
                .unwrap_err(),
            "already exists",
        );
        assert!(zfs.taken().is_empty());
        assert_eq!(storage.snapshot_tags(&p, a, RAW).await.unwrap(), ["t"]);
        // 볼륨이 없으면 할 일이 없다.
        storage.snapshot_many(&p, &[], RAW, "v").await.unwrap();
        assert!(zfs.taken().is_empty());
    }

    /// 가장 최근 스냅샷으로만 되돌린다. 크기도 그때로 돌아간다. 되돌리기 앞뒤로 장치의 버퍼를
    /// 비운다.
    #[tokio::test]
    async fn revert_only_to_the_latest_snapshot() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        storage.resize_volume(&p, id, RAW, 3 * MIB).await.unwrap();
        zfs.taken();

        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        match err {
            DriverError::InvalidSpec(msg) => assert_eq!(
                msg,
                "zfs can only revert to the latest snapshot of a volume"
            ),
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(zfs.taken().is_empty());

        storage.snapshot_revert(&p, id, RAW, "b").await.unwrap();
        assert_eq!(
            zfs.taken_all(),
            [
                format!("flushbufs {}", zvol(id)),
                format!("rollback {}@b", zvol(id)),
                "settle".into(),
                format!("flushbufs {}", zvol(id)),
            ]
        );
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, 2 * MIB);
        // 최근 스냅샷을 지우면 그 앞 스냅샷으로 되돌릴 수 있다.
        storage.snapshot_delete(&p, id, RAW, "b").await.unwrap();
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(zfs.zvol(&zvol(id)).unwrap().size, MIB);

        // 버퍼를 비우지 못하면 옛 데이터가 읽힐 수 있으므로 되돌리기가 실패한다. 되돌리기 전에
        // 비우지 못하면 되돌리지 않는다.
        zfs.taken();
        zfs.fail_on(format!("flushbufs {}", zvol(id)));
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(zfs.taken(), [format!("flushbufs {}", zvol(id))]);
    }

    /// 원본은 zvol 장치 또는 스냅샷 장치이고 크기는 그때의 것이다. 내놓을 것은 없다.
    #[tokio::test]
    async fn volume_source_reads_zvol_or_snapshot_device() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.volume_source(&p, id, RAW, None).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        zfs.taken();

        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: zfs.node(&zvol(id), Some("t")),
                format: RAW,
                snapshot: None,
                size_bytes: MIB,
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.path, zfs.node(&zvol(id), None));
        assert_eq!(source.size_bytes, 2 * MIB);
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );
        assert!(zfs.taken().is_empty());
    }

    /// 같은 백엔드 복사: 원본(스냅샷이면 그때)의 크기로 zvol을 만들고 원본을 써 넣는다. 다른
    /// 데이터셋으로도 된다. 실패하면 만든 zvol을 지운다.
    #[tokio::test]
    async fn copies_into_a_new_zvol() {
        let tools = tempfile::tempdir().unwrap();
        let (zfs, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, other) = (pool(), zfs_pool("tank/other"));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        storage.resize_volume(&p, src, RAW, 2 * MIB).await.unwrap();
        zfs.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap();
        let to = format!("tank/other/opl-{dst}");
        assert_eq!(zfs.taken(), [format!("create {to} {MIB}")]);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to_node) = (zfs.node(&zvol(src), Some("t")), zfs.node(&to, None));
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "-f",
                "raw",
                "-U",
                "-O",
                "raw",
                from.to_str().unwrap(),
                to_node.to_str().unwrap(),
            ]
        );
        let err = storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == dst),
            "{err:?}"
        );

        let (zfs, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        zfs.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            zfs.taken(),
            [
                format!("create {} {MIB}", zvol(dst)),
                format!("destroy -r {}", zvol(dst)),
            ]
        );
        assert_eq!(zfs.names(), [zvol(src)]);
    }

    /// 바꾸는 명령이 실패하면 그 오류를 그대로 돌려준다.
    #[tokio::test]
    async fn command_failures_are_zfs_errors() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        zfs.fail_on(format!("snapshot {} @t", zvol(id)));
        let err = storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert!(!err.is_transient());
    }

    /// zvol은 sparse이고 장치로만 보이며 스냅샷 장치도 보인다. 자동 스냅샷 도구(zfs-auto-snapshot
    /// 등)가 찍지 않게 한다(더 최근 스냅샷이 생기면 되돌리기가 막힌다).
    #[test]
    fn create_args_make_sparse_zvols_without_auto_snapshots() {
        assert_eq!(
            create_args("tank/opl/opl-x", "1048576"),
            [
                "create",
                "-s",
                "-V",
                "1048576",
                "-o",
                "volmode=dev",
                "-o",
                "snapdev=visible",
                "-o",
                "com.sun:auto-snapshot=false",
                "tank/opl/opl-x",
            ]
        );
    }

    /// ZFS 명령은 C 로캘로 돌고, 실패는 `Zfs` 오류다.
    #[tokio::test]
    async fn zfs_commands_use_c_locale_and_zfs_errors() {
        let long = Duration::from_secs(10);
        let out = ZFS
            .run_read("/bin/sh", &["-c", "echo $LC_ALL"], long)
            .await
            .unwrap();
        assert_eq!(out, b"C\n");
        let err = ZFS
            .run_change(
                "/bin/sh",
                &[
                    "-c",
                    "echo \"cannot open 'x': dataset does not exist\" >&2; exit 1",
                ],
                &[],
                long,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m == "/bin/sh: cannot open 'x': dataset does not exist"),
            "{err:?}"
        );
    }

    #[test]
    fn parses_zfs_output() {
        let zvols =
            parse_zvols(b"tank/opl/opl-a\t1073741824\t57344\ntank/opl/opl-b\t2097152\t8192\n\n")
                .unwrap();
        assert_eq!(
            zvols,
            [
                ZvolInfo {
                    name: "tank/opl/opl-a".into(),
                    size_bytes: 1 << 30,
                    used_bytes: 57344,
                },
                ZvolInfo {
                    name: "tank/opl/opl-b".into(),
                    size_bytes: 2 * MIB,
                    used_bytes: 8192,
                },
            ]
        );
        assert_eq!(parse_zvols(b"").unwrap(), []);
        let snapshots =
            parse_snapshots(b"tank/opl/opl-a@s1\t1048576\ntank/opl/opl-a@s2\t2097152\n").unwrap();
        assert_eq!(
            snapshots,
            [
                ZfsSnapshot {
                    tag: "s1".into(),
                    size_bytes: MIB,
                },
                ZfsSnapshot {
                    tag: "s2".into(),
                    size_bytes: 2 * MIB,
                },
            ]
        );
        assert_eq!(
            parse_space(b"6442450944\n2147483648\n").unwrap(),
            ZfsSpace {
                available_bytes: 6 << 30,
                used_bytes: 2 << 30,
            }
        );
        for err in [
            parse_zvols(b"tank/opl/opl-a\t-\t0\n").unwrap_err(),
            parse_zvols(b"tank/opl/opl-a\t1\n").unwrap_err(),
            parse_snapshots(b"tank/opl/opl-a\t1\n").unwrap_err(),
            parse_space(b"1\n").unwrap_err(),
            parse_space(b"1\t2\n3\n").unwrap_err(),
        ] {
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }
}
