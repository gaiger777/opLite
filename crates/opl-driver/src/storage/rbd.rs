//! Ceph RBD 풀: Ceph 풀(네임스페이스) 안에 볼륨마다 raw 이미지 `opl-{id}`를 둔다. QEMU가 `rbd`
//! 블록 드라이버로 이미지를 직접 연다(커널 매핑 없음). 스냅샷은 RBD 스냅샷 `opl-{id}@{tag}`다.
//! RBD 명령은 [`RbdOps`] 뒤에 있다(테스트는 가짜).

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use opl_types::runtime::{ImageFormat, RbdTarget};
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// 풀에 사용자가 없을 때 쓰는 Ceph 클라이언트 id
pub const DEFAULT_USER: &str = "admin";

/// RBD 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const RBD_TIMEOUT: Duration = Duration::from_secs(60);

/// 이미지 크기에 비례해 오래 걸리는 명령(지우기, 되돌리기)을 기다리는 시간
const RBD_LONG_TIMEOUT: Duration = Duration::from_secs(600);

/// 이미지 크기는 이 배수로 올린다(`rbd create --size`는 MiB 단위다).
const IMAGE_ALIGN: u64 = 1 << 20;

/// 이미지의 스냅샷 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbdSnapshot {
    /// `@` 뒤의 이름
    pub tag: String,
    /// 스냅샷 때의 이미지 크기
    pub size_bytes: u64,
    /// 만든 시각. 알 수 없으면 `None`.
    pub created: Option<SystemTime>,
}

/// Ceph 풀의 `ceph df`: 남은 공간(`max_avail`)과 담긴 데이터(`stored`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RbdSpace {
    pub available_bytes: u64,
    pub stored_bytes: u64,
}

/// RBD 명령. 이미지는 [`RbdTarget`](풀, 네임스페이스, 이미지, 클라이언트 id, ceph.conf)이다.
/// 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait RbdOps: Send + Sync {
    /// 이미지의 크기(`rbd info`). 이미지가 없으면 `None`.
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError>;
    /// 이미지가 쓰는 바이트(`rbd du`)
    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError>;
    /// Ceph 풀 `pool`의 공간(`ceph df detail`). `user`·`conf`로 접속한다.
    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError>;
    /// `size_bytes`(MiB 배수) 크기의 이미지를 만든다(`rbd create`).
    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 스냅샷과 함께 지운다(`rbd snap purge`, `rbd rm`).
    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError>;
    /// 이미지의 스냅샷들. 만든 순서(오래된 것부터)다.
    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError>;
    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지를 스냅샷 `tag`로 되돌린다(`rbd snap rollback`). 더 새 스냅샷은 남는다.
    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError>;
    /// 이미지 크기를 `size_bytes`(MiB 배수)로 늘린다(`rbd resize`, 줄이지 않는다).
    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError>;
    /// 이미지를 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, image: &RbdTarget) -> bool;
    /// 이미지를 연 클라이언트(watcher) 수(`rbd status`)
    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError>;
}

/// 실제 RBD 명령(`rbd`, `ceph`). 노드에 ceph.conf와 키링이 있어야 한다.
pub struct SystemRbd;

/// 같은 이미지를 바꾸는 앞 명령이 아직 돌고 있을 때의 일시 오류(`Timeout`)에 담는 말
const RBD_BUSY: &str = "an earlier rbd command still running on the same image";

/// 이미지를 연 클라이언트(다른 노드의 QEMU 등)가 있어 지우거나 되돌릴 수 없을 때의 일시 오류에 담는 말
const RBD_WATCHED: &str = "clients to close the rbd image";

/// RBD 명령. 바꾸는 명령의 대상은 이미지 사양(`{pool}[/{namespace}]/{image}`)이다. `rbd snap ls`의
/// 시각(C `ctime()`, 지역 시간)이 UTC로 나오게 `TZ=UTC`로 돌린다.
static RBD: BlockTool = BlockTool::new(
    "rbd command",
    RBD_BUSY,
    &[("LC_ALL", "C"), ("TZ", "UTC")],
    DriverError::Rbd,
);

/// 이미지 사양 `{pool}[/{namespace}]/{image}`
fn image_spec(image: &RbdTarget) -> String {
    match &image.namespace {
        Some(ns) => format!("{}/{ns}/{}", image.pool, image.image),
        None => format!("{}/{}", image.pool, image.image),
    }
}

/// Ceph 명령(`rbd`, `ceph`)의 접속 인자 `--id U [--conf C]` 뒤에 `args`를 붙인다.
fn ceph_args<'a>(user: &'a str, conf: Option<&'a str>, args: &[&'a str]) -> Vec<&'a str> {
    let mut all = vec!["--id", user];
    if let Some(conf) = conf {
        all.extend(["--conf", conf]);
    }
    all.extend_from_slice(args);
    all
}

/// 이미지 `image`에 대한 `rbd` 인자. `--no-progress`는 일부 하위 명령만 받으므로 붙이지 않는다
/// (진행률은 어차피 버리는 출력에 찍힌다).
fn rbd_args<'a>(image: &'a RbdTarget, args: &[&'a str]) -> Vec<&'a str> {
    ceph_args(&image.user, image.conf.as_deref(), args)
}

fn bad_output(what: impl std::fmt::Display) -> DriverError {
    DriverError::Rbd(format!("unparseable rbd output: {what}"))
}

fn json(out: &[u8]) -> Result<Value, DriverError> {
    serde_json::from_slice(out).map_err(bad_output)
}

/// `rbd info --format json`의 `size`
fn parse_info(out: &[u8]) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("size")
        .and_then(Value::as_u64)
        .ok_or_else(|| bad_output(format!("info {v}")))
}

/// `rbd du --format json`에서 이미지 `name`(스냅샷이 아닌 줄)의 `used_size`
fn parse_du(out: &[u8], name: &str) -> Result<u64, DriverError> {
    let v = json(out)?;
    v.get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|i| {
            i.get("name").and_then(Value::as_str) == Some(name) && i.get("snapshot").is_none()
        })
        .and_then(|i| i.get("used_size").and_then(Value::as_u64))
        .ok_or_else(|| bad_output(format!("du {v}")))
}

/// `rbd snap ls`의 `timestamp`: C `ctime()` 모양(`Thu Oct 29 09:00:00 2026`, 날이 한 자리면 공백이
/// 둘)의 UTC 시각(명령을 `TZ=UTC`로 돌린다). 읽을 수 없으면(빈 값 등) `None`.
fn parse_timestamp(text: &str) -> Option<SystemTime> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, hms, year] = parts[..] else {
        return None;
    };
    let month = MONTHS.iter().position(|m| *m == month)? as u8 + 1;
    let hms: Vec<u8> = hms
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [hour, minute, second] = hms[..] else {
        return None;
    };
    let date = time::Date::from_calendar_date(
        year.parse().ok()?,
        time::Month::try_from(month).ok()?,
        day.parse().ok()?,
    )
    .ok()?;
    let at = time::PrimitiveDateTime::new(date, time::Time::from_hms(hour, minute, second).ok()?);
    let secs = u64::try_from(at.assume_utc().unix_timestamp()).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// `rbd status --format json`의 watcher 수
fn parse_status(out: &[u8]) -> Result<usize, DriverError> {
    let v = json(out)?;
    v.get("watchers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .ok_or_else(|| bad_output(format!("status {v}")))
}

/// `rbd rm`이 이미지를 연 클라이언트 때문에 실패했으면(stderr 첫 줄이 librbd의 "image has
/// watchers"이거나 rbd의 "image still has watchers") 일시 오류로 바꾼다.
fn watched(e: DriverError) -> DriverError {
    match e {
        DriverError::Rbd(msg) if msg.contains("has watchers") => DriverError::Timeout(RBD_WATCHED),
        e => e,
    }
}

/// `rbd info`의 결과(`read`)로 이미지 크기를 낸다. 실패했으면 풀의 이미지 목록(`images`)으로
/// 이미지가 없는지 확인한다(오류 메시지로는 풀이 없는 것과 이미지가 없는 것을 가리기 어렵다). 시간을
/// 넘겼으면 목록도 늦을 것이므로 곧바로 그 오류다.
async fn info_from(
    read: Result<Vec<u8>, DriverError>,
    name: &str,
    images: impl Future<Output = Result<Vec<String>, DriverError>>,
) -> Result<Option<u64>, DriverError> {
    match read {
        Ok(out) => parse_info(&out).map(Some),
        Err(e @ DriverError::Timeout(_)) => Err(e),
        Err(e) => match images.await {
            Ok(names) if !names.iter().any(|n| n == name) => Ok(None),
            _ => Err(e),
        },
    }
}

/// `rbd snap ls --format json`. 스냅샷 id 순(만든 순서)으로 돌려준다.
fn parse_snapshots(out: &[u8]) -> Result<Vec<RbdSnapshot>, DriverError> {
    let v = json(out)?;
    let items = v
        .as_array()
        .ok_or_else(|| bad_output(format!("snap ls {v}")))?;
    let mut snapshots = items
        .iter()
        .map(|s| {
            let field = |k: &str| s.get(k).ok_or_else(|| bad_output(format!("snapshot {s}")));
            let id = field("id")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let tag = field("name")?
                .as_str()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let size = field("size")?
                .as_u64()
                .ok_or_else(|| bad_output(format!("snapshot {s}")))?;
            let created = s
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_timestamp);
            Ok((
                id,
                RbdSnapshot {
                    tag: tag.to_string(),
                    size_bytes: size,
                    created,
                },
            ))
        })
        .collect::<Result<Vec<_>, DriverError>>()?;
    snapshots.sort_by_key(|(id, _)| *id);
    Ok(snapshots.into_iter().map(|(_, s)| s).collect())
}

/// `ceph df detail --format json`에서 풀 `pool`의 `max_avail`·`stored`(없는 값은 0). 풀이 없으면 오류.
pub fn parse_ceph_df(out: &[u8], pool: &str) -> Result<RbdSpace, DriverError> {
    let v = json(out)?;
    let stats = v
        .get("pools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|p| p.get("name").and_then(Value::as_str) == Some(pool))
        .and_then(|p| p.get("stats"))
        .ok_or_else(|| DriverError::Rbd(format!("ceph pool {pool} not found in ceph df")))?;
    let field = |k: &str| stats.get(k).and_then(Value::as_u64).unwrap_or(0);
    // 풀에 바이트 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 `stored`를 뺀 것까지다.
    let (stored, quota) = (field("stored"), field("quota_bytes"));
    let mut available = field("max_avail");
    if quota > 0 {
        available = available.min(quota.saturating_sub(stored));
    }
    Ok(RbdSpace {
        available_bytes: available,
        stored_bytes: stored,
    })
}

impl SystemRbd {
    /// 이미지의 풀(네임스페이스)에 있는 이미지 이름들(`rbd ls`)
    async fn images(&self, image: &RbdTarget) -> Result<Vec<String>, DriverError> {
        let mut args = vec!["ls", "--format", "json", "--pool", &image.pool];
        if let Some(ns) = &image.namespace {
            args.extend(["--namespace", ns]);
        }
        let v = json(
            &RBD.run_read("rbd", &rbd_args(image, &args), RBD_TIMEOUT)
                .await?,
        )?;
        let names = v.as_array().ok_or_else(|| bad_output(format!("ls {v}")))?;
        Ok(names
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect())
    }

    async fn change(
        &self,
        image: &RbdTarget,
        args: &[&str],
        timeout: Duration,
    ) -> Result<(), DriverError> {
        let spec = image_spec(image);
        RBD.run_change("rbd", &rbd_args(image, args), &[&spec], timeout)
            .await
    }
}

#[async_trait]
impl RbdOps for SystemRbd {
    /// `rbd info`가 실패하면 풀의 이미지 목록으로 이미지가 없는지 확인한다([`info_from`]).
    async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["info", "--format", "json", &spec]);
        let read = RBD.run_read("rbd", &args, RBD_TIMEOUT).await;
        info_from(read, &image.image, self.images(image)).await
    }

    async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["du", "--format", "json", &spec]);
        parse_du(
            &RBD.run_read("rbd", &args, RBD_TIMEOUT).await?,
            &image.image,
        )
    }

    async fn space(
        &self,
        pool: &str,
        user: &str,
        conf: Option<&str>,
    ) -> Result<RbdSpace, DriverError> {
        let args = ceph_args(user, conf, &["df", "detail", "--format", "json"]);
        parse_ceph_df(&RBD.run_read("ceph", &args, RBD_TIMEOUT).await?, pool)
    }

    async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        let args = ["create", "--size", &size, "--image-format", "2", &spec];
        self.change(image, &args, RBD_TIMEOUT).await
    }

    async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
        let spec = image_spec(image);
        self.change(image, &["snap", "purge", &spec], RBD_LONG_TIMEOUT)
            .await?;
        self.change(image, &["rm", &spec], RBD_LONG_TIMEOUT)
            .await
            .map_err(watched)
    }

    async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["snap", "ls", "--format", "json", &spec]);
        parse_snapshots(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }

    async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "create", &name], RBD_TIMEOUT)
            .await
    }

    async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rollback", &name], RBD_LONG_TIMEOUT)
            .await
    }

    async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
        let name = format!("{}@{tag}", image_spec(image));
        self.change(image, &["snap", "rm", &name], RBD_TIMEOUT)
            .await
    }

    async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
        let (spec, size) = (image_spec(image), mib_arg(size_bytes));
        self.change(image, &["resize", "--size", &size, &spec], RBD_TIMEOUT)
            .await
    }

    fn changing(&self, image: &RbdTarget) -> bool {
        RBD.changing(&image_spec(image))
    }

    async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
        let spec = image_spec(image);
        let args = rbd_args(image, &["status", "--format", "json", &spec]);
        parse_status(&RBD.run_read("rbd", &args, RBD_TIMEOUT).await?)
    }
}

/// `rbd --size` 값: MiB 단위(`{n}M`)
fn mib_arg(size_bytes: u64) -> String {
    format!("{}M", size_bytes / IMAGE_ALIGN)
}

/// 볼륨의 이미지 이름
fn volume_image(id: Uuid) -> String {
    format!("opl-{id}")
}

/// QEMU가 여는 볼륨의 RBD 이미지. 사용자가 없으면 `admin`이다.
pub(super) fn volume_target(
    pool: &str,
    namespace: Option<&String>,
    user: Option<&String>,
    conf: Option<&String>,
    id: Uuid,
) -> RbdTarget {
    RbdTarget {
        pool: pool.to_string(),
        namespace: namespace.cloned(),
        image: volume_image(id),
        user: user.map_or(DEFAULT_USER, String::as_str).to_string(),
        conf: conf.cloned(),
    }
}

/// RBD 풀이면 그 풀의 볼륨 이미지
pub(super) fn pool_target(pool: &StoragePool, id: Uuid) -> Option<RbdTarget> {
    match &pool.spec.backend {
        PoolBackend::Rbd {
            pool,
            namespace,
            user,
            conf,
        } => Some(volume_target(
            pool,
            namespace.as_ref(),
            user.as_ref(),
            conf.as_ref(),
            id,
        )),
        PoolBackend::Dir { .. }
        | PoolBackend::Nfs { .. }
        | PoolBackend::LvmThin { .. }
        | PoolBackend::Zfs { .. } => None,
    }
}

/// RBD 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "rbd pools hold raw volumes".into(),
        )),
    }
}

/// `qemu-img`가 raw RBD 이미지(`snapshot`이 있으면 그 스냅샷)를 열게 하는 옵션 문자열
/// (`--image-opts`/`--target-image-opts`). 사용자·conf·네임스페이스를 함께 넘긴다. 옵션 값 안의
/// `,`는 `,,`로 쓴다.
pub(super) fn image_opts(image: &RbdTarget, snapshot: Option<&str>) -> String {
    let esc = |v: &str| v.replace(',', ",,");
    let mut s = format!("driver=raw,file.driver=rbd,file.pool={}", esc(&image.pool));
    if let Some(ns) = &image.namespace {
        s.push_str(&format!(",file.namespace={}", esc(ns)));
    }
    s.push_str(&format!(",file.image={}", esc(&image.image)));
    if let Some(tag) = snapshot {
        s.push_str(&format!(",file.snapshot={}", esc(tag)));
    }
    s.push_str(&format!(",file.user={}", esc(&image.user)));
    if let Some(conf) = &image.conf {
        s.push_str(&format!(",file.conf={}", esc(conf)));
    }
    s
}

/// 이미지 크기: `IMAGE_ALIGN` 배수로 올린다.
fn image_size(size_bytes: u64) -> Result<u64, DriverError> {
    size_bytes
        .div_ceil(IMAGE_ALIGN)
        .checked_mul(IMAGE_ALIGN)
        .ok_or_else(|| DriverError::InvalidSpec(format!("volume size {size_bytes} is too large")))
}

/// Ceph RBD 풀(`rbd`)의 볼륨. 볼륨은 raw 이미지이고 스냅샷은 RBD 스냅샷이다. 이미지는 노드에
/// 놓이지 않으므로 볼륨의 경로는 빈 경로다.
pub struct RbdStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn RbdOps>,
}

impl RbdStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn RbdOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
        }
    }

    /// 풀의 볼륨 이미지. RBD 풀이 아니거나 raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, id: Uuid, format: ImageFormat) -> Result<RbdTarget, DriverError> {
        require_raw(format)?;
        pool_target(pool, id).ok_or_else(|| DriverError::InvalidSpec("not an rbd pool".into()))
    }

    /// 스냅샷 태그는 RBD 스냅샷 이름과 qemu-img 옵션에 그대로 쓸 수 있는 글자여야 한다.
    fn check_tag(tag: &str) -> Result<(), DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c));
        if ok {
            Ok(())
        } else {
            Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid rbd snapshot name"
            )))
        }
    }

    /// 있는 볼륨의 크기. 없으면 `VolumeNotFound`.
    async fn existing(&self, image: &RbdTarget, id: Uuid) -> Result<u64, DriverError> {
        self.ops
            .info(image)
            .await?
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨이 있고 스냅샷 `tag`도 있어야 한다. 그 스냅샷을 돌려준다.
    async fn existing_snapshot(
        &self,
        image: &RbdTarget,
        id: Uuid,
        tag: &str,
    ) -> Result<RbdSnapshot, DriverError> {
        self.existing(image, id).await?;
        self.ops
            .snapshots(image)
            .await?
            .into_iter()
            .find(|s| s.tag == tag)
            .ok_or_else(|| {
                DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
            })
    }

    /// 이미지를 연 클라이언트(다른 노드에서 도는 VM의 QEMU 등)가 있으면 일시 오류다.
    async fn unwatched(&self, image: &RbdTarget) -> Result<(), DriverError> {
        if self.ops.watchers(image).await? > 0 {
            return Err(DriverError::Timeout(RBD_WATCHED));
        }
        Ok(())
    }

    /// 새 볼륨 이미지를 만든다. 크기는 1 MiB 배수로 올린다.
    async fn create_image(
        &self,
        image: &RbdTarget,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let size = image_size(size_bytes)?;
        if self.ops.info(image).await?.is_some() {
            return Err(DriverError::VolumeExists(id));
        }
        self.ops.create(image, size).await
    }

    /// 만든 이미지에 `qemu-img convert -n`으로 써 넣는다(`from`은 원본 인자, `-O` 없이). 새 이미지는
    /// 0으로 읽히므로 0인 부분은 쓰지 않는다(`--target-is-zero`). 실패하면 이미지를 지운다.
    async fn fill(&self, image: &RbdTarget, id: Uuid, from: &[String]) -> Result<(), DriverError> {
        let written = async {
            let dst = image_opts(image, None);
            let mut args = vec![
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
            ];
            args.extend(from.iter().map(String::as_str));
            args.push(&dst);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.ops.remove(image).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }
}

#[async_trait]
impl Storage for RbdStorage {
    /// 이미지에는 경로가 없으므로 빈 경로를 돌려준다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        self.create_image(&image, id, size_bytes).await?;
        Ok(PathBuf::new())
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let image = Self::target(pool, id, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        self.create_image(&image, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), src_str].map(String::from);
        self.fill(&image, id, &from).await?;
        Ok(PathBuf::new())
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 이미지를 스냅샷과 함께 지운다. 이미지가 없어도 그 이미지를 바꾸는 명령이 아직 돌고 있으면
    /// (시간을 넘긴 만들기) 이미지가 곧 생길 수 있으므로 `VolumeNotFound`가 아니라 일시 오류다.
    /// 이미지를 연 클라이언트가 있으면 스냅샷도 지우지 않고 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.info(&image).await?.is_none() {
            if self.ops.changing(&image) {
                return Err(DriverError::Timeout(RBD_BUSY));
            }
            return Err(DriverError::VolumeNotFound(id));
        }
        self.unwatched(&image).await?;
        self.ops.remove(&image).await
    }

    /// QEMU가 이미지를 직접 연다. 이미지가 있는지만 본다. 그 이미지를 바꾸는 명령(시간을 넘긴
    /// 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 이미지를 연 클라이언트는 보지 않는다(다시 붙는
    /// VM의 QEMU가 그 클라이언트다).
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        self.existing(&image, id).await.map(drop)
    }

    /// 쓰는 바이트(`rbd du`)를 알 수 없으면 이미지 크기로 본다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size = self.existing(&image, id).await?;
        let used = match self.ops.used(&image).await {
            Ok(used) => used,
            Err(e) => {
                tracing::debug!(volume = %id, error = %e, "rbd du failed, using image size");
                size
            }
        };
        Ok(VolumeInfo {
            virtual_bytes: size,
            allocated_bytes: used,
        })
    }

    /// Ceph 풀의 크기(`max_avail`+`stored`)와 남은 공간(`max_avail`)
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let image = Self::target(pool, Uuid::nil(), ImageFormat::Raw)?;
        let space = self
            .ops
            .space(&image.pool, &image.user, image.conf.as_deref())
            .await?;
        Ok(Capacity {
            total_bytes: space.available_bytes.saturating_add(space.stored_bytes),
            available_bytes: space.available_bytes,
        })
    }

    /// 볼륨이 raw이므로 RBD 스냅샷으로 스냅샷을 둔다.
    fn supports_snapshots(&self, _: &StoragePool, format: ImageFormat) -> bool {
        format == ImageFormat::Raw
    }

    /// VM이 실행 중이어도 만들 수 있다(일관성은 호출자가 VM을 잠시 멈춰 맞춘다). 여러 볼륨은
    /// 하나씩 찍는다(RBD에는 여러 이미지의 원자적 스냅샷이 없다).
    async fn snapshot_create(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        self.existing(&image, id).await?;
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        self.ops.snapshot(&image, tag).await
    }

    /// 아무 스냅샷으로나 되돌린다(더 새 스냅샷은 남는다). 이미지를 연 클라이언트가 없어야 한다
    /// (있으면 일시 오류).
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        self.existing_snapshot(&image, id, tag).await?;
        self.unwatched(&image).await?;
        self.ops.rollback(&image, tag).await
    }

    /// VM이 실행 중이어도 지울 수 있다. 볼륨이나 태그가 없으면 지운 것으로 본다. 다만 그 이미지를
    /// 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) 태그가 곧 생길 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let image = Self::target(pool, id, format)?;
        Self::check_tag(tag)?;
        if self.ops.info(&image).await?.is_none() {
            return Ok(());
        }
        if self.ops.changing(&image) {
            return Err(DriverError::Timeout(RBD_BUSY));
        }
        if self
            .ops
            .snapshots(&image)
            .await?
            .iter()
            .any(|s| s.tag == tag)
        {
            self.ops.remove_snapshot(&image, tag).await?;
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
        let image = Self::target(pool, id, format)?;
        self.existing(&image, id).await?;
        let snapshots = self.ops.snapshots(&image).await?;
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
        let image = Self::target(pool, id, format)?;
        if self.existing(&image, id).await? >= size_bytes {
            return Ok(());
        }
        self.ops.resize(&image, image_size(size_bytes)?).await
    }

    /// 이미지, 또는 그 스냅샷(읽기 전용). `qemu-img`가 `rbd` 드라이버로 연다. 내놓을 것은 없다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let image = Self::target(pool, id, format)?;
        let size_bytes = match tag {
            Some(tag) => self.existing_snapshot(&image, id, tag).await?.size_bytes,
            None => self.existing(&image, id).await?,
        };
        Ok(VolumeSource {
            path: PathBuf::new(),
            format,
            snapshot: tag.map(str::to_string),
            size_bytes,
            rbd: Some(image),
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

    /// 원본(스냅샷이면 그때)의 크기로 대상 이미지를 만들고 원본을 써 넣는다. Ceph 풀·사용자가
    /// 달라도 같다.
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
        let image = Self::target(dst_pool, dst_id, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        self.create_image(&image, dst_id, source.size_bytes).await?;
        let from = convert_from(&source, force_share, None)?;
        self.fill(&image, dst_id, &from).await
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
    struct FakeImage {
        size: u64,
        /// 만든 순서의 스냅샷(태그, 크기)
        snapshots: Vec<(String, u64)>,
    }

    /// Ceph 풀 `rbd`, `other`를 메모리에 두는 RBD. 이미지는 사양(`{pool}[/{ns}]/{image}`)으로
    /// 둔다. 바꾸는 호출을 적고, `fail`에 적은 호출은 실패한다.
    #[derive(Default)]
    struct FakeRbd {
        images: Mutex<BTreeMap<String, FakeImage>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        /// 바꾸는 명령이 아직 돌고 있다고 알리는 이미지 사양
        changing: Mutex<Vec<String>>,
        /// `rbd du`가 실패한다.
        no_du: Mutex<bool>,
        /// 연 클라이언트(watcher)가 있는 이미지 사양
        watched: Mutex<Vec<String>>,
    }

    impl FakeRbd {
        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Rbd(format!("injected: {call}")));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.images.lock().unwrap().keys().cloned().collect()
        }

        fn image(&self, spec: &str) -> Option<FakeImage> {
            self.images.lock().unwrap().get(spec).cloned()
        }

        fn missing(spec: &str) -> DriverError {
            DriverError::Rbd(format!(
                "rbd: error opening image {spec}: (2) No such file or directory"
            ))
        }

        fn known_pool(image: &RbdTarget) -> Result<(), DriverError> {
            if ["rbd", "other"].contains(&image.pool.as_str()) {
                Ok(())
            } else {
                Err(DriverError::Rbd(format!(
                    "rbd: error opening pool '{}': (2) No such file or directory",
                    image.pool
                )))
            }
        }
    }

    #[async_trait]
    impl RbdOps for FakeRbd {
        async fn info(&self, image: &RbdTarget) -> Result<Option<u64>, DriverError> {
            Self::known_pool(image)?;
            Ok(self.image(&image_spec(image)).map(|i| i.size))
        }

        async fn used(&self, image: &RbdTarget) -> Result<u64, DriverError> {
            if *self.no_du.lock().unwrap() {
                return Err(DriverError::Rbd("du failed".into()));
            }
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.size / 4)
        }

        async fn space(
            &self,
            pool: &str,
            user: &str,
            conf: Option<&str>,
        ) -> Result<RbdSpace, DriverError> {
            self.call(format!("df {pool} {user} {conf:?}"))?;
            Ok(RbdSpace {
                available_bytes: 6 << 30,
                stored_bytes: 2 << 30,
            })
        }

        async fn create(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("create {spec} {size_bytes}"))?;
            Self::known_pool(image)?;
            let mut images = self.images.lock().unwrap();
            if images.contains_key(&spec) {
                return Err(DriverError::Rbd(
                    "rbd: create error: (17) File exists".into(),
                ));
            }
            let i = FakeImage {
                size: size_bytes,
                snapshots: vec![],
            };
            images.insert(spec, i);
            Ok(())
        }

        async fn remove(&self, image: &RbdTarget) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("remove {spec}"))?;
            let removed = self.images.lock().unwrap().remove(&spec);
            removed.map(drop).ok_or_else(|| Self::missing(&spec))
        }

        async fn snapshots(&self, image: &RbdTarget) -> Result<Vec<RbdSnapshot>, DriverError> {
            let spec = image_spec(image);
            let i = self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            Ok(i.snapshots
                .iter()
                .map(|(tag, size)| RbdSnapshot {
                    tag: tag.clone(),
                    size_bytes: *size,
                    created: None,
                })
                .collect())
        }

        async fn snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap create {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots.push((tag.into(), i.size));
            Ok(())
        }

        async fn rollback(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rollback {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            let (_, size) = i
                .snapshots
                .iter()
                .find(|(t, _)| t == tag)
                .cloned()
                .ok_or_else(|| Self::missing(&format!("{spec}@{tag}")))?;
            i.size = size;
            Ok(())
        }

        async fn remove_snapshot(&self, image: &RbdTarget, tag: &str) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("snap rm {spec}@{tag}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            i.snapshots.retain(|(t, _)| t != tag);
            Ok(())
        }

        async fn resize(&self, image: &RbdTarget, size_bytes: u64) -> Result<(), DriverError> {
            let spec = image_spec(image);
            self.call(format!("resize {spec} {size_bytes}"))?;
            let mut images = self.images.lock().unwrap();
            let i = images.get_mut(&spec).ok_or_else(|| Self::missing(&spec))?;
            if size_bytes < i.size {
                return Err(DriverError::Rbd("shrinking requires --allow-shrink".into()));
            }
            i.size = size_bytes;
            Ok(())
        }

        fn changing(&self, image: &RbdTarget) -> bool {
            let spec = image_spec(image);
            self.changing.lock().unwrap().contains(&spec)
        }

        async fn watchers(&self, image: &RbdTarget) -> Result<usize, DriverError> {
            let spec = image_spec(image);
            self.image(&spec).ok_or_else(|| Self::missing(&spec))?;
            let watched = self.watched.lock().unwrap();
            Ok(watched.iter().filter(|s| **s == spec).count())
        }
    }

    fn rbd_pool(pool: &str, namespace: Option<&str>) -> StoragePool {
        Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: pool.into(),
                    namespace: namespace.map(Into::into),
                    user: None,
                    conf: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    fn pool() -> StoragePool {
        rbd_pool("rbd", None)
    }

    fn spec(id: Uuid) -> String {
        format!("rbd/opl-{id}")
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

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeRbd>, RbdStorage) {
        let rbd = Arc::new(FakeRbd::default());
        let storage = RbdStorage::new(qemu_img, rbd.clone());
        (rbd, storage)
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

    fn args(dir: &Path) -> Vec<String> {
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        args.lines().map(str::to_string).collect()
    }

    const RAW: ImageFormat = ImageFormat::Raw;
    const MIB: u64 = 1 << 20;

    /// 크기를 1 MiB 배수로 올린 이미지를 만든다. 이미지에는 경로가 없다.
    #[tokio::test]
    async fn creates_raw_images() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        let path = storage.create_volume(&p, id, MIB + 1, RAW).await.unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {}", spec(id), 2 * MIB)]);

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
        assert!(rbd.taken().is_empty());
        // 네임스페이스 안에 만든다. Ceph 풀이 없으면 RBD 오류다.
        let id = Uuid::now_v7();
        let ns = rbd_pool("rbd", Some("opl"));
        storage.create_volume(&ns, id, MIB, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("create rbd/opl/opl-{id} {MIB}")]);
        let err = storage
            .create_volume(&rbd_pool("none", None), id, MIB, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 원본의 가상 크기로 이미지를 만들고 raw로 변환해 써 넣는다(새 이미지는 0으로 읽힌다). 대상은
    /// 옵션 문자열이다. 실패하면 이미지를 지운다.
    #[tokio::test]
    async fn imports_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, PathBuf::new());
        assert_eq!(rbd.taken(), [format!("create {} {MIB}", spec(id))]);
        let dst =
            format!("driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{id},file.user=admin");
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "-f",
                "vmdk",
                "/up/disk.vmdk",
                &dst,
            ]
        );
        invalid(
            storage
                .import_volume(&p, id, Path::new("up.vmdk"), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(id)),
                format!("remove {}", spec(id)),
            ]
        );
        assert!(rbd.names().is_empty());
    }

    /// 이미지를 스냅샷과 함께 지운다. 없으면 `VolumeNotFound`, 그 이미지를 바꾸는 명령이 아직
    /// 돌고 있으면(시간을 넘긴 만들기) 일시 오류다.
    #[tokio::test]
    async fn delete_removes_image_with_its_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);
        not_found(storage.delete_volume(&p, id, RAW).await.unwrap_err(), id);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
    }

    /// 이미지를 연 클라이언트(watcher, 다른 노드의 QEMU 등)가 있으면 스냅샷도 지우지 않고 일시
    /// 오류다. `rbd rm`이 watcher 때문에 실패해도 일시 오류다.
    #[tokio::test]
    async fn delete_waits_for_watchers() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(err.is_transient());
        assert!(rbd.taken().is_empty());
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["t"]);

        rbd.watched.lock().unwrap().clear();
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(rbd.taken(), [format!("remove {}", spec(id))]);

        // `rbd rm`의 stderr 첫 줄(librbd 기록이나 rbd의 오류)
        for line in [
            "rbd: 2026-10-29T09:00:00.000+0000 7f0 -1 librbd::image::PreRemoveRequest: 0x1 \
             check_image_watchers: image has watchers - not removing",
            "rbd: rbd: error: image still has watchers",
        ] {
            let err = watched(DriverError::Rbd(line.into()));
            assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        }
        let err = watched(DriverError::Rbd("rbd: error: no such image".into()));
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
    }

    /// 활성화는 이미지가 있는지만 본다. 정보는 크기와 쓰는 바이트(알 수 없으면 크기), 용량은 Ceph
    /// 풀의 `max_avail`+`stored`다.
    #[tokio::test]
    async fn reports_volume_usage_and_pool_capacity() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(storage.activate_volume(&p, id, RAW).await.unwrap_err(), id);
        not_found(storage.volume_info(&p, id, RAW).await.unwrap_err(), id);
        storage.create_volume(&p, id, 4 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: MIB,
            }
        );
        *rbd.no_du.lock().unwrap() = true;
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 4 * MIB,
                allocated_bytes: 4 * MIB,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        // 사용자·conf가 있으면 그것으로 접속한다.
        let custom: StoragePool = Resource::new(
            "r",
            StoragePoolSpec {
                backend: PoolBackend::Rbd {
                    pool: "other".into(),
                    namespace: Some("ns".into()),
                    user: Some("opl".into()),
                    conf: Some("/etc/ceph/opl.conf".into()),
                },
                nodes: vec![],
                shared: true,
            },
        );
        storage.capacity(&custom).await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                "df rbd admin None",
                "df other opl Some(\"/etc/ceph/opl.conf\")"
            ]
        );
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
    }

    /// 늘리기는 1 MiB 배수로 올린다. 이미 크면 아무것도 하지 않는다.
    #[tokio::test]
    async fn resize_grows_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.resize_volume(&p, id, RAW, MIB).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, 2 * MIB, RAW).await.unwrap();
        rbd.taken();
        storage.resize_volume(&p, id, RAW, MIB).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        assert!(rbd.taken().is_empty());
        storage
            .resize_volume(&p, id, RAW, 2 * MIB + 1)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("resize {} {}", spec(id), 3 * MIB)]);
    }

    /// 스냅샷은 RBD 스냅샷이다. 같은 태그는 다시 만들지 않고, 태그는 만든 순서다. 아무 스냅샷으로나
    /// 되돌릴 수 있고 더 새 스냅샷은 남는다.
    #[tokio::test]
    async fn snapshots_are_rbd_snapshots() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        storage.snapshot_many(&p, &[id], RAW, "b").await.unwrap();
        rbd.taken();
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        invalid(
            storage
                .snapshot_create(&p, id, RAW, "a,b")
                .await
                .unwrap_err(),
            "not a valid",
        );
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );

        // 가장 최근이 아닌 스냅샷으로도 되돌린다.
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.image(&spec(id)).unwrap().size, MIB);
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );
        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        // 없는 태그와 없는 볼륨은 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "x").await.unwrap();
        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        storage
            .snapshot_delete(&p, Uuid::now_v7(), RAW, "a")
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("snap rm {}@a", spec(id))]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["b"]);

        // 그 이미지를 바꾸는 명령이 아직 돌고 있으면 태그가 곧 생길 수 있다.
        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.snapshot_delete(&p, id, RAW, "c").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// 되돌리기는 이미지를 연 클라이언트가 없어야 한다(일시 오류). 활성화(VM 시작)는 그 이미지를
    /// 바꾸는 명령(시간을 넘긴 되돌리기 등)이 아직 돌고 있으면 일시 오류다. 활성화는 watcher를 보지
    /// 않는다(다시 붙는 VM의 QEMU가 watcher다).
    #[tokio::test]
    async fn revert_and_activate_wait_for_the_image() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        rbd.taken();
        rbd.watched.lock().unwrap().push(spec(id));
        let err = storage.snapshot_revert(&p, id, RAW, "a").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_WATCHED)), "{err:?}");
        assert!(rbd.taken().is_empty());
        storage.activate_volume(&p, id, RAW).await.unwrap();
        rbd.watched.lock().unwrap().clear();
        storage.snapshot_revert(&p, id, RAW, "a").await.unwrap();
        assert_eq!(rbd.taken(), [format!("snap rollback {}@a", spec(id))]);

        rbd.changing.lock().unwrap().push(spec(id));
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
        // 만들기가 시간을 넘겨 아직 이미지가 없어도 같다.
        let new = Uuid::now_v7();
        rbd.changing.lock().unwrap().push(spec(new));
        let err = storage.activate_volume(&p, new, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(RBD_BUSY)), "{err:?}");
    }

    /// `rbd info`가 실패하면 이미지 목록으로 없는 것을 가린다. 시간을 넘겼으면 목록을 읽지 않는다.
    #[tokio::test]
    async fn info_falls_back_to_the_image_list_unless_timed_out() {
        let list = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            async move { Ok(names) }
        };
        let info = br#"{"size":1048576}"#.to_vec();
        assert_eq!(
            info_from(Ok(info), "opl-a", list(&[])).await.unwrap(),
            Some(MIB)
        );
        let failed = || Err(DriverError::Rbd("rbd: error opening image".into()));
        assert_eq!(
            info_from(failed(), "opl-a", list(&["opl-b"]))
                .await
                .unwrap(),
            None
        );
        let err = info_from(failed(), "opl-a", list(&["opl-a"]))
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Rbd(_)), "{err:?}");
        let unlisted = async { panic!("listed images after a timeout") };
        let err = info_from(Err(DriverError::Timeout("rbd command")), "opl-a", unlisted)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
    }

    /// 여러 볼륨은 하나씩 찍는다.
    #[tokio::test]
    async fn snapshot_many_takes_one_image_at_a_time() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let p = pool();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, a, MIB, RAW).await.unwrap();
        storage.create_volume(&p, b, MIB, RAW).await.unwrap();
        rbd.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        assert_eq!(
            rbd.taken(),
            [
                format!("snap create {}@t", spec(a)),
                format!("snap create {}@t", spec(b)),
            ]
        );
    }

    /// 원본은 이미지 또는 그 스냅샷이고 크기는 그때의 것이다. 내놓을 것은 없다.
    #[tokio::test]
    async fn volume_source_is_the_image_or_its_snapshot() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        not_found(
            storage.volume_source(&p, id, RAW, None).await.unwrap_err(),
            id,
        );
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        let image = pool_target(&p, id).unwrap();
        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: PathBuf::new(),
                format: RAW,
                snapshot: Some("t".into()),
                size_bytes: MIB,
                rbd: Some(image.clone()),
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!((source.snapshot, source.size_bytes), (None, 2 * MIB));
        assert_eq!(source.rbd, Some(image));
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );
        assert!(rbd.taken().is_empty());
    }

    /// 같은 백엔드 복사: 원본(스냅샷이면 그때)의 크기로 이미지를 만들고 원본을 써 넣는다. 원본과
    /// 대상 모두 옵션 문자열이다. 다른 Ceph 풀로도 된다. 실패하면 만든 이미지를 지운다.
    #[tokio::test]
    async fn copies_into_a_new_image() {
        let tools = tempfile::tempdir().unwrap();
        let (rbd, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, other) = (pool(), rbd_pool("other", Some("ns")));
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        storage.resize_volume(&p, src, RAW, 2 * MIB).await.unwrap();
        rbd.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &other, dst, RAW)
            .await
            .unwrap();
        assert_eq!(rbd.taken(), [format!("create other/ns/opl-{dst} {MIB}")]);
        let from = format!(
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-{src},file.snapshot=t,file.user=admin"
        );
        let to = format!(
            "driver=raw,file.driver=rbd,file.pool=other,file.namespace=ns,file.image=opl-{dst},file.user=admin"
        );
        assert_eq!(
            args(tools.path()),
            [
                "convert",
                "-q",
                "-n",
                "--target-is-zero",
                "--target-image-opts",
                "--image-opts",
                "-U",
                &from,
                &to,
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

        let (rbd, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, MIB, RAW).await.unwrap();
        rbd.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            rbd.taken(),
            [
                format!("create {} {MIB}", spec(dst)),
                format!("remove {}", spec(dst)),
            ]
        );
        assert_eq!(rbd.names(), [spec(src)]);
    }

    /// 다른 풀 종류는 다루지 않는다.
    #[tokio::test]
    async fn refuses_other_pools() {
        let (_, storage) = setup("/nonexistent/qemu-img");
        let dir: StoragePool = Resource::new(
            "d",
            StoragePoolSpec {
                backend: PoolBackend::Dir {
                    path: "/pools/local".into(),
                },
                nodes: vec![],
                shared: false,
            },
        );
        invalid(
            storage
                .create_volume(&dir, Uuid::now_v7(), MIB, RAW)
                .await
                .unwrap_err(),
            "not an rbd pool",
        );
        invalid(storage.capacity(&dir).await.unwrap_err(), "not an rbd pool");
    }

    /// 바꾸는 명령이 실패하면 그 오류를 그대로 돌려준다.
    #[tokio::test]
    async fn command_failures_are_rbd_errors() {
        let (rbd, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (pool(), Uuid::now_v7());
        storage.create_volume(&p, id, MIB, RAW).await.unwrap();
        rbd.fail_on(format!("snap create {}@t", spec(id)));
        let err = storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert!(!err.is_transient());
    }

    /// `qemu-img`의 옵션 문자열은 네임스페이스·스냅샷·conf를 있을 때만 넣고 `,`를 `,,`로 쓴다.
    #[test]
    fn image_opts_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(
            image_opts(&image, None),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.image=opl-a,file.user=admin"
        );
        image.namespace = Some("ns".into());
        image.user = "opl".into();
        image.conf = Some("/etc/ceph/a,b.conf".into());
        assert_eq!(
            image_opts(&image, Some("t")),
            "driver=raw,file.driver=rbd,file.pool=rbd,file.namespace=ns,file.image=opl-a,\
             file.snapshot=t,file.user=opl,file.conf=/etc/ceph/a,,b.conf"
        );
    }

    /// `rbd`·`ceph` 인자: 접속(`--id`, `--conf`)을 앞에 두고 진행률을 찍지 않는다. 이미지 사양은
    /// 네임스페이스를 담는다.
    #[test]
    fn rbd_commands_carry_user_conf_and_namespace() {
        let mut image = RbdTarget {
            pool: "rbd".into(),
            namespace: None,
            image: "opl-a".into(),
            user: "admin".into(),
            conf: None,
        };
        assert_eq!(image_spec(&image), "rbd/opl-a");
        assert_eq!(
            rbd_args(&image, &["info", "rbd/opl-a"]),
            ["--id", "admin", "info", "rbd/opl-a"]
        );
        image.namespace = Some("ns".into());
        image.conf = Some("/etc/ceph/opl.conf".into());
        assert_eq!(image_spec(&image), "rbd/ns/opl-a");
        assert_eq!(
            rbd_args(&image, &["rm", "rbd/ns/opl-a"]),
            [
                "--id",
                "admin",
                "--conf",
                "/etc/ceph/opl.conf",
                "rm",
                "rbd/ns/opl-a"
            ]
        );
        assert_eq!(
            ceph_args("opl", None, &["df", "detail"]),
            ["--id", "opl", "df", "detail"]
        );
        assert_eq!(mib_arg(3 * MIB), "3M");
    }

    /// RBD 명령은 C 로캘로 돌고, 실패는 `Rbd` 오류다.
    #[tokio::test]
    async fn rbd_commands_use_c_locale_and_rbd_errors() {
        let long = Duration::from_secs(10);
        let out = RBD
            .run_read("/bin/sh", &["-c", "echo $LC_ALL $TZ"], long)
            .await
            .unwrap();
        // `rbd snap ls`의 시각(C `ctime()`)이 UTC로 나온다.
        assert_eq!(out, b"C UTC\n");
        let err = RBD
            .run_read("/bin/sh", &["-c", "echo 'rbd: error' >&2; exit 2"], long)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Rbd(ref m) if m == "/bin/sh: rbd: error"),
            "{err:?}"
        );
    }

    #[test]
    fn parses_rbd_output() {
        assert_eq!(
            parse_info(br#"{"name":"opl-a","size":1073741824,"objects":256}"#).unwrap(),
            1 << 30
        );
        assert!(parse_info(b"{}").is_err());
        assert!(parse_info(b"not json").is_err());

        // 스냅샷 줄이 아닌 이미지 줄의 used_size
        let du = br#"{"images":[
            {"name":"opl-a","snapshot":"t","id":4,"provisioned_size":10,"used_size":7},
            {"name":"opl-a","id":"1","provisioned_size":10,"used_size":3},
            {"name":"opl-b","id":"2","provisioned_size":10,"used_size":9}
        ],"total_provisioned_size":20,"total_used_size":19}"#;
        assert_eq!(parse_du(du, "opl-a").unwrap(), 3);
        assert!(parse_du(du, "opl-c").is_err());

        // 스냅샷 id 순. 시각은 읽을 수 있을 때만 담는다.
        let snaps = br#"[
            {"id":9,"name":"b","size":2048,"protected":"false","timestamp":"x"},
            {"id":4,"name":"a","size":1024,"protected":"false","timestamp":"Thu Oct  1 09:00:05 2026"},
            {"id":5,"name":"c","size":1024,"protected":"false"}
        ]"#;
        assert_eq!(
            parse_snapshots(snaps).unwrap(),
            [
                RbdSnapshot {
                    tag: "a".into(),
                    size_bytes: 1024,
                    created: Some(UNIX_EPOCH + Duration::from_secs(1_790_845_205)),
                },
                RbdSnapshot {
                    tag: "c".into(),
                    size_bytes: 1024,
                    created: None,
                },
                RbdSnapshot {
                    tag: "b".into(),
                    size_bytes: 2048,
                    created: None,
                },
            ]
        );
        assert_eq!(parse_snapshots(b"[]").unwrap(), []);
        assert!(parse_snapshots(br#"[{"id":1}]"#).is_err());

        let df = br#"{"stats":{},"pools":[
            {"name":"other","id":1,"stats":{"stored":1,"max_avail":2}},
            {"name":"rbd","id":2,"stats":{"stored":300,"max_avail":700,"objects":3}},
            {"name":"empty","id":3,"stats":{}}
        ]}"#;
        assert_eq!(
            parse_ceph_df(df, "rbd").unwrap(),
            RbdSpace {
                available_bytes: 700,
                stored_bytes: 300,
            }
        );
        assert_eq!(
            parse_ceph_df(df, "empty").unwrap(),
            RbdSpace {
                available_bytes: 0,
                stored_bytes: 0,
            }
        );
        assert!(matches!(
            parse_ceph_df(df, "none").unwrap_err(),
            DriverError::Rbd(_)
        ));
        // 쿼터(`quota_bytes`, 0이면 없음)가 있으면 남은 공간은 쿼터에서 담긴 데이터를 뺀 것까지다.
        let df = br#"{"pools":[
            {"name":"q","stats":{"stored":300,"max_avail":700,"quota_bytes":500}},
            {"name":"full","stats":{"stored":600,"max_avail":700,"quota_bytes":500}},
            {"name":"wide","stats":{"stored":300,"max_avail":700,"quota_bytes":5000}},
            {"name":"none","stats":{"stored":300,"max_avail":700,"quota_bytes":0}}
        ]}"#;
        let available = |pool| parse_ceph_df(df, pool).unwrap().available_bytes;
        assert_eq!(available("q"), 200);
        assert_eq!(available("full"), 0);
        assert_eq!(available("wide"), 700);
        assert_eq!(available("none"), 700);
        assert_eq!(parse_ceph_df(df, "q").unwrap().stored_bytes, 300);

        // `rbd status`의 watcher 수
        let status = br#"{"watchers":[
            {"address":"10.0.0.1:0/1","client":4100,"cookie":1},
            {"address":"10.0.0.2:0/2","client":4200,"cookie":2}
        ]}"#;
        assert_eq!(parse_status(status).unwrap(), 2);
        assert_eq!(parse_status(br#"{"watchers":[]}"#).unwrap(), 0);
        assert!(parse_status(b"{}").is_err());
    }

    /// `ctime()` 모양의 UTC 시각. 읽을 수 없으면 `None`이다.
    #[test]
    fn parses_ctime_timestamps() {
        let at = |secs| Some(UNIX_EPOCH + Duration::from_secs(secs));
        assert_eq!(
            parse_timestamp("Thu Oct 29 09:00:00 2026"),
            at(1_793_264_400)
        );
        assert_eq!(parse_timestamp("Thu Jan  1 00:00:00 1970"), at(0));
        assert_eq!(parse_timestamp("Mon Feb 29 23:59:59 2027"), None);
        for bad in [
            "",
            "x",
            "Thu Oct 29 09:00 2026",
            "Thu Okt 29 09:00:00 2026",
            "Thu Oct 29 25:00:00 2026",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }
}
