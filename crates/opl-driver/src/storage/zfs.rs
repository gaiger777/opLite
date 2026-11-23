//! ZFS 풀: 데이터셋 바로 아래에 볼륨마다 sparse zvol `{dataset}/opl-{id}`(raw)를 두고, 스냅샷은 ZFS
//! 스냅샷 `{dataset}/opl-{id}@{tag}`로 둔다. ZFS 명령은 [`ZfsOps`] 뒤에 있다(테스트는 가짜).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{BASE_TAG, PoolBackend, SourceFormat, StoragePool};
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, ImportSource, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    run_qemu_img, source_virtual_size,
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

/// 평탄화가 보낼 때 찍는 복제 zvol의 스냅샷. 받은 쪽에서는 끝에 지운다.
const FLATTEN_TAG: &str = "opl-flatten";

/// 평탄화가 받는 새 zvol 이름의 꼬리(`{dataset}/opl-{id}-flat`). 원본을 지운 뒤 원본 이름으로 바꾼다.
const FLAT_SUFFIX: &str = "-flat";

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

/// `zfs get -Hp -o value available,used,type`로 읽은 데이터셋(opld 마운터가 풀을 확인한다)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZfsDataset {
    /// `type`: `filesystem`, `volume` 등
    pub kind: String,
    pub space: ZfsSpace,
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
    /// zvol이 clone이면 그 원본 스냅샷(`{zvol}@{tag}`, `zfs get origin`), 아니면 `None`.
    async fn origin(&self, zvol: &str) -> Result<Option<String>, DriverError>;
    /// 스냅샷 `{zvol}@{tag}` 뒤에 zvol에 쓴 바이트(`zfs get written@{tag}`)
    async fn written(&self, zvol: &str, tag: &str) -> Result<u64, DriverError>;
    /// 스냅샷 `snapshot`(`{zvol}@{tag}`)의 clone zvol `zvol`을 만든다(`zfs clone`, 새 zvol과 같은 속성).
    async fn clone_snapshot(&self, snapshot: &str, zvol: &str) -> Result<(), DriverError>;
    /// zvol의 이름을 바꾼다(`zfs rename`).
    async fn rename(&self, from: &str, to: &str) -> Result<(), DriverError>;
    /// zvol `zvol`의 스냅샷 `tag`를 `target`으로 보낸다(`zfs send | zfs recv`). `from`이 없으면 전체 스트림으로 새
    /// zvol `target`을 만들고(새 zvol과 같은 속성), 있으면 `from` 다음부터 `tag`까지의 스냅샷을 모두(`-I`) 이미
    /// 받은 `target`에 더한다. 크기에 비례해 걸리므로 시간 제한이 없다.
    async fn send(
        &self,
        zvol: &str,
        from: Option<&str>,
        tag: &str,
        target: &str,
    ) -> Result<(), DriverError>;
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

/// `zfs get -Hp -o value available,used,type`의 출력
pub fn parse_zfs_dataset(out: &[u8]) -> Result<ZfsDataset, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [available, used, kind] if [available, used, kind].iter().all(|v| v.len() == 1) => {
            Ok(ZfsDataset {
                kind: kind[0].trim().to_string(),
                space: ZfsSpace {
                    available_bytes: number(available[0])?,
                    used_bytes: number(used[0])?,
                },
            })
        }
        _ => Err(bad_output(format!("dataset {values:?}"))),
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

    async fn origin(&self, zvol: &str) -> Result<Option<String>, DriverError> {
        let args = ["get", "-Hp", "-o", "value", "origin", zvol];
        parse_origin(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn written(&self, zvol: &str, tag: &str) -> Result<u64, DriverError> {
        let property = format!("written@{tag}");
        let args = ["get", "-Hp", "-o", "value", &property, zvol];
        parse_written(&ZFS.run_read("zfs", &args, ZFS_TIMEOUT).await?)
    }

    async fn clone_snapshot(&self, snapshot: &str, zvol: &str) -> Result<(), DriverError> {
        let mut args = vec!["clone"];
        args.extend_from_slice(&ZVOL_PROPS);
        args.extend([snapshot, zvol]);
        ZFS.run_change("zfs", &args, &[zvol], ZFS_TIMEOUT).await
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), DriverError> {
        ZFS.run_change("zfs", &["rename", from, to], &[from, to], ZFS_TIMEOUT)
            .await
    }

    async fn send(
        &self,
        zvol: &str,
        from: Option<&str>,
        tag: &str,
        target: &str,
    ) -> Result<(), DriverError> {
        let (snapshot, from) = (format!("{zvol}@{tag}"), from.map(|f| format!("@{f}")));
        let (send, recv) = send_recv_args(&snapshot, from.as_deref(), target);
        ZFS.run_pipe_change(
            ("zfs", &send),
            ("zfs", &recv),
            &[zvol, target],
            Duration::MAX,
        )
        .await
    }
}

/// 새 zvol의 속성(`-o` 인자): 장치로만 보이고(`volmode=dev`) 스냅샷 장치도 보인다(`snapdev=visible`). 이 속성을 따르는
/// 자동 스냅샷 도구(zfs-auto-snapshot 등)가 찍지 않게 `com.sun:auto-snapshot=false`를 둔다: 더 최근 스냅샷이 생기면
/// 되돌리기가 막힌다.
const ZVOL_PROPS: [&str; 6] = [
    "-o",
    "volmode=dev",
    "-o",
    "snapdev=visible",
    "-o",
    "com.sun:auto-snapshot=false",
];

/// `zfs send [-I @from] {snapshot}`과 `zfs recv`의 인자. 전체 스트림은 새 zvol의 속성으로 받는다.
fn send_recv_args<'a>(
    snapshot: &'a str,
    from: Option<&'a str>,
    target: &'a str,
) -> (Vec<&'a str>, Vec<&'a str>) {
    let mut send = vec!["send"];
    let mut recv = vec!["recv"];
    match from {
        Some(from) => send.extend(["-I", from]),
        None => recv.extend_from_slice(&ZVOL_PROPS),
    }
    send.push(snapshot);
    recv.push(target);
    (send, recv)
}

/// `zfs get -Hp -o value origin`의 출력. clone이 아니면 `-`다.
fn parse_origin(out: &[u8]) -> Result<Option<String>, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [value] if value.len() == 1 => {
            let origin = value[0].trim();
            Ok((origin != "-" && !origin.is_empty()).then(|| origin.to_string()))
        }
        _ => Err(bad_output(format!("origin {values:?}"))),
    }
}

/// `zfs get -Hp -o value written@{tag}`의 출력
fn parse_written(out: &[u8]) -> Result<u64, DriverError> {
    let values: Vec<Vec<&str>> = rows(out).collect();
    match values.as_slice() {
        [value] if value.len() == 1 => number(value[0].trim()),
        _ => Err(bad_output(format!("written {values:?}"))),
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
    /// 원본의 가상 크기로 zvol을 만들고 raw로 변환해 써 넣는다.
    async fn import_from(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: ImportSource<'_>,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let from = src.args(Some("raw"))?;
        let size = source_virtual_size(&self.qemu_img, src).await?;
        let dst = self.create_zvol(dataset, id, size).await?;
        self.fill(dataset, id, &dst, &from).await?;
        Ok(dst)
    }

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
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Rbd { .. }
            | PoolBackend::LvmShared { .. } => {
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
        self.retry_busy(zvol, || self.ops.destroy(zvol)).await
    }

    /// zvol의 스냅샷 `tag`를 지운다. "busy"로 실패하면 [`BUSY_RETRIES`]번까지 다시 시도한다.
    async fn destroy_snapshot(&self, zvol: &str, tag: &str) -> Result<(), DriverError> {
        self.retry_busy(zvol, || self.ops.destroy_snapshot(zvol, tag))
            .await
    }

    /// 지우기 `destroy`가 "busy"로 실패하면 [`BUSY_RETRIES`]번까지 다시 시도한다.
    async fn retry_busy<F, Fut>(&self, zvol: &str, destroy: F) -> Result<(), DriverError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<(), DriverError>>,
    {
        let mut retries = 0;
        loop {
            match destroy().await {
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
        self.import_from(pool, id, ImportSource::File(src, src_format), format)
            .await
    }

    async fn import_chain(
        &self,
        pool: &StoragePool,
        id: Uuid,
        chain: &[PathBuf],
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.import_from(pool, id, ImportSource::Chain(chain), format)
            .await
    }

    async fn import_nbd(
        &self,
        pool: &StoragePool,
        id: Uuid,
        socket: &Path,
        export: &str,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        self.import_from(pool, id, ImportSource::Nbd(socket, export), format)
            .await
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// zvol을 스냅샷과 함께 지운다. 먼저 udev를 기다린다(방금 생긴 스냅샷 장치를 열고 있을 수
    /// 있다). zvol이 없어도 그 zvol을 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 만들기) zvol이 곧
    /// 생길 수 있으므로 `VolumeNotFound`가 아니라 일시 오류다. 끊긴 평탄화가 남긴 `{zvol}-flat`도 지운다(원본을
    /// 지운 뒤 이름을 바꾸기 전에 끊겼으면 볼륨의 데이터는 그것뿐이다).
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = volume_zvol(dataset, id);
        let flat = format!("{zvol}{FLAT_SUFFIX}");
        let has_flat = self.find(dataset, &flat).await?.is_some();
        if self.find(dataset, &zvol).await?.is_none() {
            if self.ops.changing(&zvol) || self.ops.changing(&flat) {
                return Err(DriverError::Timeout(ZFS_BUSY));
            }
            if !has_flat {
                return Err(DriverError::VolumeNotFound(id));
            }
        } else {
            self.settle().await;
            self.destroy(&zvol).await?;
        }
        if has_flat {
            self.settle().await;
            self.destroy(&flat).await?;
        }
        Ok(())
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
        // 스냅샷 장치 노드가 생기지만(`snapdev=visible`) udev를 기다리지 않는다: VM이 멈춘 동안일 수
        // 있고 `udevadm settle`은 호스트의 udev 이벤트를 모두 기다린다. 쓰거나 지울 때 기다린다.
        self.ops.snapshot(&zvols, tag).await
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
    /// 지우기 전에 udev를 기다린다(방금 생긴 스냅샷 장치를 열고 있을 수 있다).
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
            self.settle().await;
            self.destroy_snapshot(&zvol, tag).await?;
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
            rbd: None,
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
        let from = convert_from(&source, force_share, Some(dst_format))?;
        self.fill(dataset, dst_id, &dst, &from).await
    }

    /// 기반 zvol에 기반 스냅샷 `@opl-base`가 없으면 찍고 그 스냅샷의 clone `{dataset}/opl-{clone}`을 만든다. 장치
    /// 노드가 생기지 않으면 만든 clone을 지운다. 기반 zvol은 clone이 있는 동안 지울 수 없다(`zfs destroy`가 거절).
    async fn link_clone(
        &self,
        pool: &StoragePool,
        base_id: Uuid,
        base_format: ImageFormat,
        clone_id: Uuid,
    ) -> Result<(PathBuf, ImageFormat), DriverError> {
        let dataset = Self::dataset(pool, base_format)?;
        let base = self.existing(dataset, base_id).await?.name;
        let clone = volume_zvol(dataset, clone_id);
        if self.find(dataset, &clone).await?.is_some() {
            return Err(DriverError::VolumeExists(clone_id));
        }
        if !self
            .ops
            .snapshots(&base)
            .await?
            .iter()
            .any(|s| s.tag == BASE_TAG)
        {
            self.ops.snapshot(&[&base], BASE_TAG).await?;
        }
        self.ops
            .clone_snapshot(&format!("{base}@{BASE_TAG}"), &clone)
            .await?;
        match self.wait_node(device_path(&self.dev, &clone, None)).await {
            Ok(path) => Ok((path, ImageFormat::Raw)),
            Err(e) => {
                if let Err(del) = self.destroy(&clone).await {
                    tracing::warn!(volume = %clone_id, error = %del, "cannot remove clone without device");
                }
                Err(e)
            }
        }
    }

    /// 기반 스냅샷 `@opl-base`를 지운다. clone이 남아 있으면 `zfs destroy`가 거절한다. 볼륨이나 스냅샷이 없으면
    /// 지운 것으로 본다. 다만 그 zvol을 바꾸는 명령이 아직 돌고 있으면 일시 오류다.
    async fn release_base(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        self.snapshot_delete(pool, id, format, BASE_TAG).await
    }

    /// clone zvol을 기반과 독립인 zvol로 바꾼다(`zfs promote`는 의존 방향을 뒤집으므로 쓰지 않는다). clone의 스냅샷
    /// `@opl-flatten`을 찍어 그때까지의 스냅샷과 함께 `{zvol}-flat`으로 보내고(`zfs send | zfs recv`, 첫 스냅샷은 전체
    /// 스트림), 원본 clone을 지운 뒤 `-flat`을 원본 이름으로 바꾸고 `@opl-flatten`을 지운다. 다시 할 때: 원본과
    /// `-flat`이 함께 있으면 `-flat`을 지우고 처음부터, 원본 없이 `-flat`만 있으면 이름만 바꾼다. 이미 clone이 아니면
    /// 남은 `@opl-flatten`만 지운다.
    async fn flatten_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let dataset = Self::dataset(pool, format)?;
        let zvol = volume_zvol(dataset, id);
        let flat = format!("{zvol}{FLAT_SUFFIX}");
        if self.ops.changing(&zvol) || self.ops.changing(&flat) {
            return Err(DriverError::Timeout(ZFS_BUSY));
        }
        let volumes = self.ops.volumes(dataset).await?;
        let has = |name: &str| volumes.iter().any(|v| v.name == name);
        match (has(&zvol), has(&flat)) {
            (false, false) => return Err(DriverError::VolumeNotFound(id)),
            (false, true) => return self.finish_flatten(&flat, &zvol).await,
            (true, true) => {
                tracing::warn!(volume = %id, "removing a flattened copy left by an earlier attempt");
                self.settle().await;
                self.destroy(&flat).await?;
            }
            (true, false) => {}
        }
        let snapshots = self.ops.snapshots(&zvol).await?;
        if self.ops.origin(&zvol).await?.is_none() {
            if snapshots.iter().any(|s| s.tag == FLATTEN_TAG) {
                self.settle().await;
                self.destroy_snapshot(&zvol, FLATTEN_TAG).await?;
            }
            return Ok(());
        }
        // 앞선 시도가 남긴 스냅샷은 그 뒤의 쓰기를 담지 않는다.
        if snapshots.iter().any(|s| s.tag == FLATTEN_TAG) {
            self.settle().await;
            self.destroy_snapshot(&zvol, FLATTEN_TAG).await?;
        }
        self.ops.snapshot(&[&zvol], FLATTEN_TAG).await?;
        let first = snapshots
            .iter()
            .map(|s| s.tag.as_str())
            .find(|t| *t != FLATTEN_TAG)
            .unwrap_or(FLATTEN_TAG);
        let sent = async {
            self.ops.send(&zvol, None, first, &flat).await?;
            if first != FLATTEN_TAG {
                self.ops
                    .send(&zvol, Some(first), FLATTEN_TAG, &flat)
                    .await?;
            }
            Ok::<_, DriverError>(())
        };
        if let Err(e) = sent.await {
            // 원본은 그대로다. 받다 만 것은 다시 할 때도 지운다.
            self.settle().await;
            if self.find(dataset, &flat).await.is_ok_and(|f| f.is_some())
                && let Err(del) = self.destroy(&flat).await
            {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial flattened copy");
            }
            return Err(e);
        }
        // 보낸 시점 뒤에 쓰였으면(그사이 VM이 켜졌다) 받은 것은 그 쓰기를 담지 않으므로 원본을 지우지 않는다.
        let written = self.ops.written(&zvol, FLATTEN_TAG).await?;
        if written > 0 {
            self.settle().await;
            if let Err(del) = self.destroy(&flat).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove stale flattened copy");
            }
            return Err(DriverError::Zfs(format!(
                "{zvol} was written while it was being flattened; stop what uses it and flatten it again"
            )));
        }
        self.settle().await;
        self.destroy(&zvol).await?;
        self.finish_flatten(&flat, &zvol).await
    }
}

impl ZfsStorage {
    /// 평탄화의 끝: 받은 `flat`을 원본 이름 `zvol`로 바꾸고 보낼 때 찍은 스냅샷을 지운 뒤 장치 노드를 기다린다.
    async fn finish_flatten(&self, flat: &str, zvol: &str) -> Result<(), DriverError> {
        self.ops.rename(flat, zvol).await?;
        self.settle().await;
        if self
            .ops
            .snapshots(zvol)
            .await?
            .iter()
            .any(|s| s.tag == FLATTEN_TAG)
        {
            self.destroy_snapshot(zvol, FLATTEN_TAG).await?;
        }
        self.wait_node(device_path(&self.dev, zvol, None))
            .await
            .map(drop)
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
        /// clone이면 원본 스냅샷(`{zvol}@{tag}`)
        origin: Option<String>,
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
        /// `written`이 알릴 바이트(zvol마다, 없으면 0)
        written: Mutex<BTreeMap<String, u64>>,
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
                written: Mutex::default(),
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

        /// `snapshot`(`{zvol}@{tag}`, `tag`가 없으면 그 zvol의 모든 스냅샷)을 원본으로 둔 clone이 있으면 거절
        fn check_dependents(
            zvols: &BTreeMap<String, FakeZvol>,
            zvol: &str,
            tag: Option<&str>,
        ) -> Result<(), DriverError> {
            let origin_of = |o: &str| match tag {
                Some(tag) => o == format!("{zvol}@{tag}"),
                None => o.starts_with(&format!("{zvol}@")),
            };
            if zvols
                .values()
                .any(|z| z.origin.as_deref().is_some_and(origin_of))
            {
                return Err(DriverError::Zfs(format!(
                    "cannot destroy '{zvol}': filesystem has dependent clones"
                )));
            }
            Ok(())
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
                origin: None,
            };
            zvols.insert(zvol.into(), z);
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn destroy(&self, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("destroy -r {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            Self::check_dependents(&zvols, zvol, None)?;
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
            Self::check_dependents(&zvols, zvol, Some(tag))?;
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

        async fn origin(&self, zvol: &str) -> Result<Option<String>, DriverError> {
            let zvols = self.zvols.lock().unwrap();
            Ok(zvols
                .get(zvol)
                .ok_or_else(|| Self::missing(zvol))?
                .origin
                .clone())
        }

        async fn written(&self, zvol: &str, _: &str) -> Result<u64, DriverError> {
            Ok(self.written.lock().unwrap().get(zvol).copied().unwrap_or(0))
        }

        async fn clone_snapshot(&self, snapshot: &str, zvol: &str) -> Result<(), DriverError> {
            self.call(format!("clone {snapshot} {zvol}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            let (origin, tag) = snapshot.split_once('@').unwrap();
            let size = zvols
                .get(origin)
                .and_then(|z| z.snapshots.iter().find(|(t, _)| t == tag))
                .map(|(_, size)| *size)
                .ok_or_else(|| Self::missing(snapshot))?;
            if zvols.contains_key(zvol) {
                return Err(DriverError::Zfs(format!("cannot create '{zvol}': exists")));
            }
            let z = FakeZvol {
                size,
                snapshots: vec![],
                origin: Some(snapshot.into()),
            };
            zvols.insert(zvol.into(), z);
            self.sync_nodes(&zvols);
            Ok(())
        }

        async fn rename(&self, from: &str, to: &str) -> Result<(), DriverError> {
            self.call(format!("rename {from} {to}"))?;
            let mut zvols = self.zvols.lock().unwrap();
            if zvols.contains_key(to) {
                return Err(DriverError::Zfs(format!("cannot rename to '{to}': exists")));
            }
            let z = zvols.remove(from).ok_or_else(|| Self::missing(from))?;
            zvols.insert(to.into(), z);
            self.sync_nodes(&zvols);
            Ok(())
        }

        /// 받은 zvol은 clone이 아니다. 전체 스트림은 `tag` 하나를, `-I`는 `from` 뒤 `tag`까지의 스냅샷을 더한다.
        async fn send(
            &self,
            zvol: &str,
            from: Option<&str>,
            tag: &str,
            target: &str,
        ) -> Result<(), DriverError> {
            let call = match from {
                Some(from) => format!("send -I @{from} {zvol}@{tag} | recv {target}"),
                None => format!("send {zvol}@{tag} | recv {target}"),
            };
            self.call(call)?;
            let mut zvols = self.zvols.lock().unwrap();
            let source = zvols.get(zvol).ok_or_else(|| Self::missing(zvol))?.clone();
            let end = source
                .snapshots
                .iter()
                .position(|(t, _)| t == tag)
                .ok_or_else(|| Self::missing(&format!("{zvol}@{tag}")))?;
            match from {
                None => {
                    if zvols.contains_key(target) {
                        return Err(DriverError::Zfs(format!("'{target}' exists")));
                    }
                    let snapshot = source.snapshots[end].clone();
                    let z = FakeZvol {
                        size: snapshot.1,
                        snapshots: vec![snapshot],
                        origin: None,
                    };
                    zvols.insert(target.into(), z);
                }
                Some(from) => {
                    let start = source
                        .snapshots
                        .iter()
                        .position(|(t, _)| t == from)
                        .ok_or_else(|| Self::missing(&format!("{zvol}@{from}")))?;
                    let t = zvols.get_mut(target).ok_or_else(|| Self::missing(target))?;
                    if t.snapshots.last().map(|(t, _)| t.as_str()) != Some(from) {
                        return Err(DriverError::Zfs(
                            "most recent snapshot does not match".into(),
                        ));
                    }
                    t.snapshots
                        .extend_from_slice(&source.snapshots[start + 1..=end]);
                    t.size = source.snapshots[end].1;
                }
            }
            self.sync_nodes(&zvols);
            Ok(())
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
                backups: false,
                folder: None,
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
        let path = dir.join("fake-qemu-img");
        let args = dir.join("args");
        let end = if fail { "echo broken >&2\nexit 1" } else { "" };
        crate::storage::tests::write_script(
            &path,
            &format!(
                "#!/bin/sh\nif [ \"$1\" = info ]; then echo '{{\"virtual-size\": 1000}}'; exit 0; fi\n\
                 printf '%s\\n' \"$@\" > {args:?}\n{end}\n"
            ),
        );
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
    /// 연결 복제는 기반 스냅샷 `@opl-base`(없을 때만 찍는다)의 clone이다. clone이 있는 동안 기반 zvol도 기반
    /// 스냅샷도 지울 수 없다. 기반 스냅샷 풀기는 다시 해도 된다.
    #[tokio::test]
    async fn linked_clones_are_clones_of_the_base_snapshot() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (base, a, b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, base, MIB, RAW).await.unwrap();
        zfs.taken();

        let (path, format) = storage.link_clone(&p, base, RAW, a).await.unwrap();
        assert_eq!((path, format), (zfs.node(&zvol(a), None), RAW));
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {} @opl-base", zvol(base)),
                format!("clone {}@opl-base {}", zvol(base), zvol(a)),
            ]
        );
        storage.link_clone(&p, base, RAW, b).await.unwrap();
        assert_eq!(
            zfs.taken(),
            [format!("clone {}@opl-base {}", zvol(base), zvol(b))]
        );
        assert_eq!(
            zfs.zvol(&zvol(b)).unwrap().origin,
            Some(format!("{}@opl-base", zvol(base)))
        );

        let err = storage.link_clone(&p, base, RAW, a).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == a),
            "{err:?}"
        );
        let missing = Uuid::now_v7();
        not_found(
            storage
                .link_clone(&p, missing, RAW, Uuid::now_v7())
                .await
                .unwrap_err(),
            missing,
        );
        invalid(
            storage
                .link_clone(&p, base, ImageFormat::Qcow2, Uuid::now_v7())
                .await
                .unwrap_err(),
            "raw volumes",
        );
        zfs.taken();

        // 백엔드 막이: clone이 있으면 기반 zvol도 기반 스냅샷도 지워지지 않는다.
        let err = storage.delete_volume(&p, base, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("dependent clones")),
            "{err:?}"
        );
        let err = storage.release_base(&p, base, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("dependent clones")),
            "{err:?}"
        );
        assert!(zfs.names().contains(&zvol(base)));

        for clone in [a, b] {
            storage.delete_volume(&p, clone, RAW).await.unwrap();
        }
        zfs.taken();
        storage.release_base(&p, base, RAW).await.unwrap();
        assert_eq!(zfs.taken(), [format!("destroy {}@opl-base", zvol(base))]);
        assert!(zfs.zvol(&zvol(base)).unwrap().snapshots.is_empty());
        storage.release_base(&p, base, RAW).await.unwrap();
        storage.release_base(&p, missing, RAW).await.unwrap();
        assert!(zfs.taken().is_empty());
    }

    /// 평탄화는 clone의 스냅샷들을 `@opl-flatten`과 함께 `-flat`으로 보내고(첫 스냅샷은 전체, 나머지는 `-I`) 원본을
    /// 지운 뒤 이름을 바꾼다. 평탄화한 볼륨은 clone이 아니고 스냅샷이 남는다. 다시 하면 할 일이 없다.
    #[tokio::test]
    async fn flatten_sends_the_clone_into_an_independent_zvol() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (base, id) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, base, MIB, RAW).await.unwrap();
        storage.link_clone(&p, base, RAW, id).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "s1").await.unwrap();
        storage.snapshot_create(&p, id, RAW, "s2").await.unwrap();
        zfs.taken();

        storage.flatten_volume(&p, id, RAW).await.unwrap();
        let (z, flat) = (zvol(id), format!("{}-flat", zvol(id)));
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {z} @opl-flatten"),
                format!("send {z}@s1 | recv {flat}"),
                format!("send -I @s1 {z}@opl-flatten | recv {flat}"),
                format!("destroy -r {z}"),
                format!("rename {flat} {z}"),
                format!("destroy {z}@opl-flatten"),
            ]
        );
        let flattened = zfs.zvol(&z).unwrap();
        assert_eq!(flattened.origin, None);
        let tags: Vec<&str> = flattened
            .snapshots
            .iter()
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(tags, ["s1", "s2"]);
        assert!(zfs.node(&z, None).exists());
        // 이제 기반 스냅샷을 풀 수 있다.
        storage.release_base(&p, base, RAW).await.unwrap();
        zfs.taken();

        storage.flatten_volume(&p, id, RAW).await.unwrap();
        assert!(zfs.taken().is_empty());
        let missing = Uuid::now_v7();
        not_found(
            storage.flatten_volume(&p, missing, RAW).await.unwrap_err(),
            missing,
        );
    }

    /// 스냅샷 없는 clone은 `@opl-flatten` 하나를 전체 스트림으로 보낸다. 보내다 실패하면 받다 만 것을 지우고 원본은
    /// 그대로다. 다시 할 때 원본과 `-flat`이 함께 있으면 `-flat`을 지우고 처음부터, `-flat`만 있으면 이름만 바꾼다.
    #[tokio::test]
    async fn flatten_resumes_after_interruptions() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (base, id) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, base, MIB, RAW).await.unwrap();
        storage.link_clone(&p, base, RAW, id).await.unwrap();
        let (z, flat) = (zvol(id), format!("{}-flat", zvol(id)));
        let base_snapshot = format!("{}@opl-base", zvol(base));

        // 받다 실패: 원본은 clone 그대로다(남은 `@opl-flatten`은 다음에 새로 찍는다).
        zfs.fail_on(format!("send {z}@opl-flatten | recv {flat}"));
        zfs.taken();
        storage.flatten_volume(&p, id, RAW).await.unwrap_err();
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {z} @opl-flatten"),
                format!("send {z}@opl-flatten | recv {flat}"),
            ]
        );
        assert_eq!(
            zfs.zvol(&z).unwrap().origin.as_deref(),
            Some(base_snapshot.as_str())
        );
        zfs.fail.lock().unwrap().clear();

        // 원본을 지우다 실패(쓰는 중): `-flat`이 남는다. 다시 하면 지우고 처음부터 한다.
        zfs.fail_on(format!("destroy -r {z}"));
        storage.flatten_volume(&p, id, RAW).await.unwrap_err();
        assert!(zfs.names().contains(&flat));
        zfs.fail.lock().unwrap().clear();
        zfs.taken();
        storage.flatten_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            zfs.taken(),
            [
                format!("destroy -r {flat}"),
                format!("destroy {z}@opl-flatten"),
                format!("snapshot {z} @opl-flatten"),
                format!("send {z}@opl-flatten | recv {flat}"),
                format!("destroy -r {z}"),
                format!("rename {flat} {z}"),
                format!("destroy {z}@opl-flatten"),
            ]
        );
        assert_eq!(zfs.zvol(&z).unwrap().origin, None);

        // 원본을 지운 뒤 이름을 바꾸기 전에 끊겼으면 이름만 바꾼다.
        let other = Uuid::now_v7();
        let (oz, oflat) = (zvol(other), format!("{}-flat", zvol(other)));
        storage.link_clone(&p, base, RAW, other).await.unwrap();
        zfs.fail_on(format!("rename {oflat} {oz}"));
        storage.flatten_volume(&p, other, RAW).await.unwrap_err();
        assert_eq!(zfs.names().iter().filter(|n| **n == oz).count(), 0);
        zfs.fail.lock().unwrap().clear();
        zfs.taken();
        storage.flatten_volume(&p, other, RAW).await.unwrap();
        assert_eq!(
            zfs.taken(),
            [
                format!("rename {oflat} {oz}"),
                format!("destroy {oz}@opl-flatten")
            ]
        );
        assert!(zfs.zvol(&oz).unwrap().snapshots.is_empty());
    }

    /// 보낸 시점 뒤에 clone이 쓰였으면(그사이 VM이 켜졌다) 원본을 지우지 않고 받은 것을 지운 뒤 실패한다.
    #[tokio::test]
    async fn flatten_keeps_a_clone_written_while_sending() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (base, id) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, base, MIB, RAW).await.unwrap();
        storage.link_clone(&p, base, RAW, id).await.unwrap();
        let (z, flat) = (zvol(id), format!("{}-flat", zvol(id)));
        zfs.written.lock().unwrap().insert(z.clone(), 4096);
        zfs.taken();
        let err = storage.flatten_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("was written while it was being flattened")),
            "{err:?}"
        );
        assert_eq!(
            zfs.taken(),
            [
                format!("snapshot {z} @opl-flatten"),
                format!("send {z}@opl-flatten | recv {flat}"),
                format!("destroy -r {flat}"),
            ]
        );
        assert!(zfs.zvol(&z).unwrap().origin.is_some());
        assert!(!zfs.names().contains(&flat));
    }

    /// 지우기는 끊긴 평탄화가 남긴 `-flat`도 지운다: 원본과 함께 있어도, 원본을 지운 뒤 끊겨 `-flat`만 있어도.
    #[tokio::test]
    async fn delete_removes_a_flattened_copy_left_behind() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (base, a, b) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, base, MIB, RAW).await.unwrap();
        storage.link_clone(&p, base, RAW, a).await.unwrap();
        storage.link_clone(&p, base, RAW, b).await.unwrap();
        // a: 원본을 지우다 실패해 둘 다 남음
        let (az, aflat) = (zvol(a), format!("{}-flat", zvol(a)));
        zfs.fail_on(format!("destroy -r {az}"));
        storage.flatten_volume(&p, a, RAW).await.unwrap_err();
        zfs.fail.lock().unwrap().clear();
        // b: 원본을 지운 뒤 이름을 바꾸다 실패해 `-flat`만 남음
        let (bz, bflat) = (zvol(b), format!("{}-flat", zvol(b)));
        zfs.fail_on(format!("rename {bflat} {bz}"));
        storage.flatten_volume(&p, b, RAW).await.unwrap_err();
        zfs.fail.lock().unwrap().clear();
        assert!(zfs.names().contains(&aflat) && zfs.names().contains(&bflat));
        zfs.taken();

        storage.delete_volume(&p, a, RAW).await.unwrap();
        storage.delete_volume(&p, b, RAW).await.unwrap();
        assert_eq!(
            zfs.taken(),
            [
                format!("destroy -r {az}"),
                format!("destroy -r {aflat}"),
                format!("destroy -r {bflat}"),
            ]
        );
        let names = zfs.names();
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with(&az) || n.starts_with(&bz)),
            "{names:?}"
        );
        not_found(storage.delete_volume(&p, b, RAW).await.unwrap_err(), b);
    }

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
        // 스냅샷 장치 노드를 udev가 아직 다루고 있을 수 있으므로 먼저 기다린다.
        assert_eq!(
            zfs.taken_all(),
            ["settle".into(), format!("destroy -r {}", zvol(id))]
        );
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

    /// 스냅샷 지우기는 udev를 먼저 기다리고(방금 생긴 스냅샷 장치를 udev가 열고 있을 수 있다)
    /// "busy"면 세 번까지 다시 시도한다.
    #[tokio::test]
    async fn snapshot_delete_settles_and_retries_while_busy() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let destroy = format!("destroy {}@t", zvol(id));
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 3);
        storage.snapshot_delete(&p, id, RAW, "t").await.unwrap();
        let mut want = vec!["settle".to_string()];
        want.extend(vec![destroy.clone(); 4]);
        assert_eq!(zfs.taken_all(), want);
        assert!(storage.snapshot_tags(&p, id, RAW).await.unwrap().is_empty());

        // 네 번째도 busy면 그 오류다.
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        zfs.taken();
        zfs.busy_on(&destroy, 4);
        let err = storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Zfs(ref m) if m.contains("busy")),
            "{err:?}"
        );
        assert_eq!(zfs.taken(), vec![destroy.clone(); 4]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["t"]);
    }

    /// zvol이 아직 없어도 그 zvol을 바꾸는 명령이 돌고 있으면(시간을 넘긴 만들기) 지우기는 일시
    /// 오류다. `VolumeNotFound`로 자원을 지우면 명령이 끝나 생긴 zvol이 남는다.
    #[tokio::test]
    async fn delete_waits_for_a_running_create() {
        let (zfs, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        zfs.changing.lock().unwrap().push(zvol(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        assert!(zfs.taken().is_empty());
        zfs.changing.lock().unwrap().clear();
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);
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
        // VM이 멈춘 동안이므로 udev를 기다리지 않는다(지울 때 기다린다).
        assert_eq!(
            zfs.taken_all(),
            [format!("snapshot {} {} @t", zvol(a), zvol(b))]
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
                rbd: None,
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
    fn parses_origin_and_builds_send_recv_args() {
        assert_eq!(parse_origin(b"-\n").unwrap(), None);
        assert_eq!(
            parse_origin(b"tank/opl/opl-a@opl-base\n").unwrap(),
            Some("tank/opl/opl-a@opl-base".into())
        );
        parse_origin(b"").unwrap_err();
        assert_eq!(parse_written(b"0\n").unwrap(), 0);
        assert_eq!(parse_written(b"65536\n").unwrap(), 65536);
        parse_written(b"").unwrap_err();
        let (send, recv) = send_recv_args("t/o-a@s1", None, "t/o-a-flat");
        assert_eq!(send, ["send", "t/o-a@s1"]);
        assert_eq!(
            recv,
            [
                "recv",
                "-o",
                "volmode=dev",
                "-o",
                "snapdev=visible",
                "-o",
                "com.sun:auto-snapshot=false",
                "t/o-a-flat"
            ]
        );
        let (send, recv) = send_recv_args("t/o-a@opl-flatten", Some("@s1"), "t/o-a-flat");
        assert_eq!(send, ["send", "-I", "@s1", "t/o-a@opl-flatten"]);
        assert_eq!(recv, ["recv", "t/o-a-flat"]);
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

    #[test]
    fn parses_zfs_dataset() {
        assert_eq!(
            parse_zfs_dataset(b"6442450944\n2147483648\nfilesystem\n").unwrap(),
            ZfsDataset {
                kind: "filesystem".into(),
                space: ZfsSpace {
                    available_bytes: 6 << 30,
                    used_bytes: 2 << 30,
                },
            }
        );
        for bad in [&b"1\n2\n"[..], b"1\nx\nfilesystem\n", b"1\n2\nvolume\n3\n"] {
            let err = parse_zfs_dataset(bad).unwrap_err();
            assert!(matches!(err, DriverError::Zfs(_)), "{err:?}");
        }
    }
}
