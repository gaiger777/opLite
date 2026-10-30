//! LVM-thin 풀: VG의 thin pool 안에 볼륨마다 thin LV `opl-{id}`(raw)를 두고, 스냅샷은 thin 스냅샷
//! LV `opl-{id}-s-{tag}`로 둔다. 공유 LVM 풀: 공유(lvmlockd) VG 안에 볼륨마다 두꺼운 LV `opl-{id}`
//! (raw)를 두고 쓰는 노드에서만 배타 활성화한다. LVM 명령은 [`LvmOps`] 뒤에 있다(테스트는 가짜).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use opl_types::runtime::ImageFormat;
use opl_types::storage::{PoolBackend, SourceFormat, StoragePool};
use serde_json::Value;
use uuid::Uuid;

use super::blockcmd::BlockTool;
use super::{
    Capacity, Storage, VolumeInfo, VolumeSource, check_image, convert_from, path_str,
    require_absolute, run_qemu_img, source_virtual_size, src_fmt_name,
};
use crate::error::DriverError;

/// LVM 명령 하나를 기다리는 시간. 지나면 `Timeout`(일시 오류)이다.
const LVM_TIMEOUT: Duration = Duration::from_secs(60);

/// LV 장치 노드가 놓이는 디렉터리(`/dev/{vg}/{lv}`)
pub(super) const DEV: &str = "/dev";

/// `lvs`가 알려 준 LV 하나
#[derive(Debug, Clone, PartialEq)]
pub struct LvInfo {
    pub name: String,
    /// `lv_attr`(예: thin pool `twi-aotz--`, thin 볼륨 `Vwi-a-tz--`)
    pub attr: String,
    pub size_bytes: u64,
    /// thin pool과 thin 볼륨의 데이터 사용률(%). 없으면(비활성 등) `None`.
    pub data_percent: Option<f64>,
}

impl LvInfo {
    pub fn is_thin_pool(&self) -> bool {
        self.attr.starts_with('t')
    }

    /// 장치 노드가 있는지(`lv_attr`의 다섯째 글자 `a`)
    pub fn is_active(&self) -> bool {
        self.attr.chars().nth(4) == Some('a')
    }

    /// thin pool이 새로 할당한 블록을 쓰기 전에 0으로 채우는지(`lv_attr`의 여덟째 글자 `z`,
    /// `lvcreate -Zn`이면 아니다). 아니면 thin LV의 덜 쓴 블록에 옛 데이터가 보일 수 있다.
    pub fn zeroes_new_blocks(&self) -> bool {
        self.attr.chars().nth(7) == Some('z')
    }

    /// 데이터 사용률로 계산한 할당 바이트
    pub fn used_bytes(&self) -> u64 {
        let percent = self.data_percent.unwrap_or(0.0).clamp(0.0, 100.0);
        (self.size_bytes as f64 * percent / 100.0) as u64
    }
}

/// `lvs --reportformat json --units b --nosuffix -o lv_name,lv_attr,lv_size,data_percent`의 출력을
/// 읽는다. 값은 문자열로 오지만 숫자도 받는다.
pub fn parse_lvs(out: &[u8]) -> Result<Vec<LvInfo>, DriverError> {
    let bad = |what: String| DriverError::Lvm(format!("unparseable lvs output: {what}"));
    let json: Value = serde_json::from_slice(out).map_err(|e| bad(e.to_string()))?;
    let reports = json
        .get("report")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no report".into()))?;
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut lvs = Vec::new();
    for lv in reports
        .iter()
        .filter_map(|r| r.get("lv").and_then(Value::as_array))
        .flatten()
    {
        let field = |k: &str| lv.get(k).and_then(text);
        let (Some(name), Some(attr), Some(size)) =
            (field("lv_name"), field("lv_attr"), field("lv_size"))
        else {
            return Err(bad(format!("lv without name, attr or size: {lv}")));
        };
        let size_bytes = size.parse().map_err(|_| bad(format!("lv_size {size:?}")))?;
        let data_percent = match field("data_percent").as_deref() {
            None | Some("") => None,
            Some(p) => Some(p.parse().map_err(|_| bad(format!("data_percent {p:?}")))?),
        };
        lvs.push(LvInfo {
            name,
            attr,
            size_bytes,
            data_percent,
        });
    }
    Ok(lvs)
}

/// `vgs`가 알려 준 VG 하나
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgInfo {
    pub name: String,
    /// 잠금 종류(`vg_lock_type`). 공유 VG면 `sanlock`·`dlm`, 아니면 빈 문자열이다.
    pub lock_type: String,
    pub size_bytes: u64,
    pub free_bytes: u64,
}

/// `vgs --reportformat json --units b --nosuffix -o vg_name,vg_lock_type,vg_size,vg_free`의 출력을
/// 읽는다. 값은 문자열로 오지만 숫자도 받는다.
pub fn parse_vgs(out: &[u8]) -> Result<Vec<VgInfo>, DriverError> {
    let bad = |what: String| DriverError::Lvm(format!("unparseable vgs output: {what}"));
    let json: Value = serde_json::from_slice(out).map_err(|e| bad(e.to_string()))?;
    let reports = json
        .get("report")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no report".into()))?;
    let text = |v: &Value| match v {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    };
    let mut vgs = Vec::new();
    for vg in reports
        .iter()
        .filter_map(|r| r.get("vg").and_then(Value::as_array))
        .flatten()
    {
        let field = |k: &str| vg.get(k).and_then(text);
        let (Some(name), Some(size), Some(free)) =
            (field("vg_name"), field("vg_size"), field("vg_free"))
        else {
            return Err(bad(format!("vg without name, size or free: {vg}")));
        };
        let number = |k: &str, v: &str| v.parse().map_err(|_| bad(format!("{k} {v:?}")));
        vgs.push(VgInfo {
            name,
            lock_type: field("vg_lock_type").unwrap_or_default(),
            size_bytes: number("vg_size", &size)?,
            free_bytes: number("vg_free", &free)?,
        });
    }
    Ok(vgs)
}

/// `lvs --noheadings -o lv_tags`의 출력(쉼표로 나눈 태그)을 읽는다.
fn parse_lv_tags(out: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(out)
        .trim()
        .split(',')
        .filter(|t| !t.is_empty())
        .map(String::from)
        .collect()
}

/// LVM 명령. 이름은 VG 안의 LV 이름이다. 테스트는 가짜로 바꾼다.
#[async_trait]
pub trait LvmOps: Send + Sync {
    /// VG의 LV들(`lvs`)
    async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError>;
    /// thin pool에 `size_bytes` 크기의 thin 볼륨을 만든다(`lvcreate -T`).
    async fn create_thin(
        &self,
        vg: &str,
        thinpool: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError>;
    /// `origin`의 thin 스냅샷을 만든다(`lvcreate -s`). 활성화 건너뛰기 표시가 붙는다.
    async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError>;
    async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 크기를 `size_bytes`로 늘린다(`lvextend`).
    async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError>;
    async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError>;
    /// 활성화(건너뛰기 표시가 있어도) 또는 비활성화(`lvchange -ay -K` / `-an`)
    async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError>;
    /// 활성화 건너뛰기 표시를 지운다(`lvchange -kn`).
    async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// LV `name`을 바꾸는 명령이 아직 돌고 있는지(시간을 넘겨 뒤에서 도는 중)
    fn changing(&self, vg: &str, name: &str) -> bool;
    /// `size_bytes` 크기의 두꺼운(linear) LV를 켜지 않고 만든다(`lvcreate -an`).
    async fn create_linear(&self, vg: &str, name: &str, size_bytes: u64)
    -> Result<(), DriverError>;
    /// 이 노드에서만 켠다(`lvchange -aey`). 공유 VG에서 다른 노드가 켜 두었으면 lvmlockd가 거절한다.
    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError>;
    /// 켜 둔 LV의 내용을 `offset` 바이트부터 끝까지 0으로 채운다(`blkdiscard -z`). 크기에 비례해
    /// 걸리므로 시간 제한이 없다.
    async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError>;
    /// LV의 태그(`lvs -o lv_tags`)
    async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError>;
    /// 태그를 붙이거나 뗀다(`lvchange --addtag` / `--deltag`).
    async fn set_tag(&self, vg: &str, name: &str, tag: &str, on: bool) -> Result<(), DriverError>;
    /// VG의 잠금 종류와 크기·남은 공간(`vgs`)
    async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError>;
}

/// 실제 LVM 명령. root여야 한다.
pub struct SystemLvm;

/// LVM 명령. 숫자를 `.`로 쓰고, 물려받은 파일 기술자 경고를 stderr에 섞지 않는다. 바꾸는 명령의
/// 대상은 `{vg}/{lv}`다.
static LVM: BlockTool = BlockTool::new(
    "lvm command",
    "an earlier lvm command still running on the same lv",
    &[("LC_ALL", "C"), ("LVM_SUPPRESS_FD_WARNINGS", "1")],
    DriverError::Lvm,
)
.preferring(&[LV_HELD_BY, VG_HELD_BY]);

impl SystemLvm {
    /// VG `vg`의 LV `lvs`를 바꾸는 명령([`BlockTool::run_change`])
    async fn change(
        program: &str,
        args: &[&str],
        vg: &str,
        lvs: &[&str],
    ) -> Result<(), DriverError> {
        let lvs: Vec<String> = lvs.iter().map(|lv| format!("{vg}/{lv}")).collect();
        let lvs: Vec<&str> = lvs.iter().map(String::as_str).collect();
        LVM.run_change(program, args, &lvs, LVM_TIMEOUT).await
    }
}

#[async_trait]
impl LvmOps for SystemLvm {
    async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError> {
        let out = LVM
            .run_read(
                "lvs",
                &[
                    "--reportformat",
                    "json",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "lv_name,lv_attr,lv_size,data_percent",
                    vg,
                ],
                LVM_TIMEOUT,
            )
            .await?;
        parse_lvs(&out)
    }

    async fn create_thin(
        &self,
        vg: &str,
        thinpool: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let (pool, size) = (format!("{vg}/{thinpool}"), format!("{size_bytes}b"));
        let args = ["-q", "-T", &pool, "-V", &size, "-n", name, "-y"];
        Self::change("lvcreate", &args, vg, &[name]).await
    }

    async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError> {
        let origin = format!("{vg}/{origin}");
        Self::change("lvcreate", &["-q", "-s", "-n", name, &origin], vg, &[name]).await
    }

    async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvremove", &["-q", "-y", &lv], vg, &[name]).await
    }

    async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError> {
        let (lv, size) = (format!("{vg}/{name}"), format!("{size_bytes}b"));
        Self::change("lvextend", &["-q", "-L", &size, &lv], vg, &[name]).await
    }

    async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError> {
        Self::change("lvrename", &["-q", vg, from, to], vg, &[from, to]).await
    }

    async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        let args: &[&str] = if active {
            &["-q", "-ay", "-K", &lv]
        } else {
            &["-q", "-an", &lv]
        };
        Self::change("lvchange", args, vg, &[name]).await
    }

    async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvchange", &["-q", "-kn", &lv], vg, &[name]).await
    }

    fn changing(&self, vg: &str, name: &str) -> bool {
        LVM.changing(&format!("{vg}/{name}"))
    }

    /// 켜지 않으므로 처음 부분을 0으로 채우거나 서명을 지우지 않는다(`-Zn -Wn`). 내용은 뒤에
    /// [`zero`](LvmOps::zero)가 모두 0으로 채운다.
    async fn create_linear(
        &self,
        vg: &str,
        name: &str,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let size = format!("{size_bytes}b");
        let args = ["-q", "-an", "-Zn", "-Wn", "-L", &size, "-n", name, vg];
        Self::change("lvcreate", &args, vg, &[name]).await
    }

    async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        Self::change("lvchange", &["-q", "-aey", &lv], vg, &[name]).await
    }

    /// 새 LV에 옛 데이터의 서명이 남아 있어도 묻지 않는다(`-f`).
    async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError> {
        let device = device_node(Path::new(DEV), vg, name)?;
        let (lv, offset) = (format!("{vg}/{name}"), offset.to_string());
        let args = ["-f", "-z", "-o", &offset, path_str(&device)?];
        LVM.run_change("blkdiscard", &args, &[&lv], Duration::MAX)
            .await
    }

    async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError> {
        let lv = format!("{vg}/{name}");
        let out = LVM
            .run_read("lvs", &["--noheadings", "-o", "lv_tags", &lv], LVM_TIMEOUT)
            .await?;
        Ok(parse_lv_tags(&out))
    }

    async fn set_tag(&self, vg: &str, name: &str, tag: &str, on: bool) -> Result<(), DriverError> {
        let lv = format!("{vg}/{name}");
        let op = if on { "--addtag" } else { "--deltag" };
        Self::change("lvchange", &["-q", op, tag, &lv], vg, &[name]).await
    }

    async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError> {
        let out = LVM
            .run_read(
                "vgs",
                &[
                    "--reportformat",
                    "json",
                    "--units",
                    "b",
                    "--nosuffix",
                    "-o",
                    "vg_name,vg_lock_type,vg_size,vg_free",
                    vg,
                ],
                LVM_TIMEOUT,
            )
            .await?;
        parse_vgs(&out)?
            .into_iter()
            .find(|v| v.name == vg)
            .ok_or_else(|| DriverError::Lvm(format!("volume group {vg} not found")))
    }
}

/// 볼륨의 LV 이름
fn volume_lv(id: Uuid) -> String {
    format!("opl-{id}")
}

/// 볼륨 스냅샷 LV 이름의 앞부분
fn snapshot_prefix(id: Uuid) -> String {
    format!("opl-{id}-s-")
}

/// 되돌리는 동안 옛 볼륨 LV를 옮겨 두는 이름의 앞부분. 되돌리기가 실패하면 남을 수 있다.
fn revert_prefix(id: Uuid) -> String {
    format!("opl-{id}-revert-")
}

/// LV 장치 노드 `{dev}/{vg}/{lv}`
pub(super) fn device_path(dev: &Path, vg: &str, lv: &str) -> PathBuf {
    dev.join(vg).join(lv)
}

/// 있는 LV 장치 노드. udev가 만드는 `{dev}/{vg}/{lv}`가 없으면 device-mapper 노드
/// `{dev}/mapper/{vg}-{lv}`(이름 안의 `-`는 `--`)를 쓴다. 둘 다 없으면 `VolumeNotActive`(비활성이거나
/// 노드가 아직 생기지 않았다).
pub(super) fn device_node(dev: &Path, vg: &str, lv: &str) -> Result<PathBuf, DriverError> {
    let path = device_path(dev, vg, lv);
    if path.exists() {
        return Ok(path);
    }
    let dm = format!("{}-{}", vg.replace('-', "--"), lv.replace('-', "--"));
    let mapper = dev.join("mapper").join(dm);
    if mapper.exists() {
        return Ok(mapper);
    }
    Err(DriverError::VolumeNotActive(path))
}

/// LVM-thin 볼륨의 장치 노드([`device_node`])
pub(super) fn volume_device(dev: &Path, vg: &str, id: Uuid) -> Result<PathBuf, DriverError> {
    device_node(dev, vg, &volume_lv(id))
}

/// LVM-thin 풀은 raw 볼륨만 담는다.
pub(super) fn require_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "lvm-thin pools hold raw volumes".into(),
        )),
    }
}

/// 새로 만든 볼륨 LV
struct NewLv {
    /// 장치 노드
    path: PathBuf,
    /// 쓰기 전에 0으로 읽히는지(thin pool이 새 블록을 0으로 채운다)
    reads_zero: bool,
}

/// LVM-thin 풀(`lvm-thin`)의 볼륨. 볼륨은 thin LV이고 스냅샷은 thin 스냅샷 LV다.
pub struct LvmThinStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn LvmOps>,
    /// LV 장치 노드가 놓이는 디렉터리([`DEV`]). 테스트는 바꾼다.
    dev: PathBuf,
}

impl LvmThinStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn LvmOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(DEV),
        }
    }

    /// 풀의 VG와 thin pool. raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, format: ImageFormat) -> Result<(&str, &str), DriverError> {
        require_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::LvmThin { vg, thinpool } => Ok((vg, thinpool)),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. }
            | PoolBackend::LvmShared { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-thin pool".into()))
            }
        }
    }

    /// 스냅샷 LV 이름. 태그는 LVM 이름에 쓸 수 있는 글자여야 한다.
    fn snapshot_lv(id: Uuid, tag: &str) -> Result<String, DriverError> {
        let ok = !tag.is_empty()
            && tag
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+_.-".contains(&c));
        if !ok {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot tag {tag:?} is not a valid lvm name"
            )));
        }
        Ok(format!("{}{tag}", snapshot_prefix(id)))
    }

    /// VG의 LV 중 `name`
    async fn find(&self, vg: &str, name: &str) -> Result<Option<LvInfo>, DriverError> {
        Ok(self
            .ops
            .list(vg)
            .await?
            .into_iter()
            .find(|lv| lv.name == name))
    }

    /// VG의 LV들. 볼륨 LV가 없고 되돌리기가 옮겨 둔 `-revert-` LV가 하나만 있으면(시간을 넘긴
    /// 이름 바꾸기가 되돌리기가 실패한 뒤에 끝났다) 그 LV가 볼륨이므로 이름을 되돌린 뒤 다시 읽는다.
    /// 둘 중 하나라도 바꾸는 명령이 아직 돌고 있으면 일시 오류(`Timeout`)이고, `-revert-` LV가
    /// 여럿이면 어느 것인지 모르므로 `Lvm`이다(수동 복구).
    async fn volume_lvs(&self, vg: &str, id: Uuid) -> Result<Vec<LvInfo>, DriverError> {
        let lvs = self.ops.list(vg).await?;
        let volume = volume_lv(id);
        if lvs.iter().any(|lv| lv.name == volume) {
            return Ok(lvs);
        }
        let prefix = revert_prefix(id);
        let reverts: Vec<&str> = lvs
            .iter()
            .map(|lv| lv.name.as_str())
            .filter(|name| name.starts_with(&prefix))
            .collect();
        let old = match reverts.as_slice() {
            [] => return Ok(lvs),
            [old] => old.to_string(),
            _ => {
                return Err(DriverError::Lvm(format!(
                    "volume {id} is missing and several lvs may hold it ({}); rename the right \
                     one to {vg}/{volume}",
                    reverts.join(", ")
                )));
            }
        };
        if self.ops.changing(vg, &old) || self.ops.changing(vg, &volume) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        tracing::warn!(volume = %id, lv = %old, "restoring volume left by an unfinished revert");
        if let Err(e) = self.ops.rename(vg, &old, &volume).await
            && !self.done_after(&e, vg, &volume, false).await
        {
            return Err(e);
        }
        self.ops.list(vg).await
    }

    /// 있는 볼륨 LV([`volume_lvs`](Self::volume_lvs)). 없으면 `VolumeNotFound`.
    async fn existing(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        self.volume_lvs(vg, id)
            .await?
            .into_iter()
            .find(|lv| lv.name == volume_lv(id))
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨이 있고 스냅샷 LV `tag`도 있어야 한다. 스냅샷 LV를 돌려준다.
    async fn existing_snapshot(
        &self,
        vg: &str,
        id: Uuid,
        tag: &str,
    ) -> Result<LvInfo, DriverError> {
        let name = Self::snapshot_lv(id, tag)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        lvs.into_iter().find(|lv| lv.name == name).ok_or_else(|| {
            DriverError::InvalidSpec(format!("snapshot {tag} not found on volume {id}"))
        })
    }

    /// VG `vg`의 thin pool `thinpool`. 없거나 thin pool이 아니면 `Lvm`.
    async fn thin_pool(&self, vg: &str, thinpool: &str) -> Result<LvInfo, DriverError> {
        self.find(vg, thinpool)
            .await?
            .filter(LvInfo::is_thin_pool)
            .ok_or_else(|| DriverError::Lvm(format!("thin pool {vg}/{thinpool} not found")))
    }

    /// 새 볼륨 LV를 만든다. 크기는 512 배수로 올린다(LVM은 extent 단위로 다시 올린다). 장치 노드가
    /// 없으면 만든 LV를 지운다.
    async fn create_lv(
        &self,
        vg: &str,
        thinpool: &str,
        id: Uuid,
        size_bytes: u64,
    ) -> Result<NewLv, DriverError> {
        let lvs = self.ops.list(vg).await?;
        if lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeExists(id));
        }
        let reads_zero = lvs
            .iter()
            .any(|lv| lv.name == thinpool && lv.is_thin_pool() && lv.zeroes_new_blocks());
        let size = size_bytes.div_ceil(512) * 512;
        self.ops
            .create_thin(vg, thinpool, &volume_lv(id), size)
            .await?;
        match volume_device(&self.dev, vg, id) {
            Ok(path) => Ok(NewLv { path, reads_zero }),
            Err(e) => {
                if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
                    tracing::warn!(volume = %id, error = %del, "cannot remove volume without device");
                }
                Err(e)
            }
        }
    }

    /// 만든 볼륨 LV에 `qemu-img convert -n`으로 써 넣는다. 실패하면 볼륨 LV를 지운다.
    /// 새 LV가 0으로 읽히면 0인 부분은 쓰지 않는다(`--target-is-zero`).
    async fn fill(
        &self,
        vg: &str,
        id: Uuid,
        dst: &NewLv,
        from: &[String],
    ) -> Result<(), DriverError> {
        let written = async {
            let mut args = vec!["convert", "-q", "-n"];
            if dst.reads_zero {
                args.push("--target-is-zero");
            }
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(&dst.path)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
                tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
            }
            return Err(e);
        }
        Ok(())
    }

    /// 시간을 넘긴 명령(`e`가 `Timeout`) 뒤에 LV `name`이 있는지(`active`면 활성인지) 다시 본다.
    /// `lvs`는 VG 잠금을 기다리므로 그 명령이 끝난 뒤의 상태다. 다른 오류거나 볼 수 없으면 `false`.
    async fn done_after(&self, e: &DriverError, vg: &str, name: &str, active: bool) -> bool {
        e.may_be_in_progress()
            && matches!(self.find(vg, name).await, Ok(Some(lv)) if !active || lv.is_active())
    }

    /// `volume_source`로 읽게 한 복사 원본을 내놓을 가드([`release_source`](Storage::release_source)와
    /// 같다: 스냅샷이면 그 LV를 비활성화한다).
    fn source_guard(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<SourceGuard, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = tag.map(|tag| Self::snapshot_lv(id, tag)).transpose()?;
        Ok(SourceGuard {
            ops: self.ops.clone(),
            vg: vg.to_string(),
            lv,
        })
    }

    /// 되돌리기가 중간에 실패했을 때 옛 볼륨 LV `old`의 이름을 되돌린다. 되돌리지 못하면 남은 LV
    /// 이름을 오류에 적는다(수동 복구).
    async fn restore_name(&self, vg: &str, id: Uuid, old: &str, cause: DriverError) -> DriverError {
        match self.ops.rename(vg, old, &volume_lv(id)).await {
            Ok(()) => cause,
            Err(e) => DriverError::Lvm(format!(
                "revert of volume {id} failed ({cause}) and the volume is left as {vg}/{old}: {e}"
            )),
        }
    }
}

/// 복사 원본으로 활성화한 스냅샷 LV(`lv`, 스냅샷이 아니면 `None`). `release`로 내놓지 않고 버려지면
/// (복사 future가 취소됨) 뒤에서 비활성화한다.
struct SourceGuard {
    ops: Arc<dyn LvmOps>,
    vg: String,
    lv: Option<String>,
}

impl SourceGuard {
    /// 원본을 내놓는다. 복사는 끝났으므로 실패해도 로그만 남긴다(활성화된 채 남을 뿐이다).
    async fn release(mut self) {
        if let Some(lv) = self.lv.take() {
            deactivate(&*self.ops, &self.vg, &lv).await;
        }
    }
}

impl Drop for SourceGuard {
    fn drop(&mut self) {
        let Some(lv) = self.lv.take() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (ops, vg) = (self.ops.clone(), std::mem::take(&mut self.vg));
        rt.spawn(async move { deactivate(&*ops, &vg, &lv).await });
    }
}

async fn deactivate(ops: &dyn LvmOps, vg: &str, lv: &str) {
    if let Err(e) = ops.set_active(vg, lv, false).await {
        tracing::warn!(lv = %format!("{vg}/{lv}"), error = %e, "cannot release copy source");
    }
}

#[async_trait]
impl Storage for LvmThinStorage {
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let (vg, thinpool) = Self::target(pool, format)?;
        Ok(self.create_lv(vg, thinpool, id, size_bytes).await?.path)
    }

    /// 원본의 가상 크기로 볼륨 LV를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let (vg, thinpool) = Self::target(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let dst = self.create_lv(vg, thinpool, id, size).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(vg, id, &dst, &from).await?;
        Ok(dst.path)
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 볼륨 LV를 먼저 지운다(쓰고 있으면 실패하고 스냅샷은 그대로 남는다). 그다음 스냅샷 LV와
    /// 실패한 되돌리기가 남긴 LV를 지운다. 볼륨 LV가 없어도(지우다 멈춘 뒤 다시 지움) 남은 것을
    /// 지우고 `VolumeNotFound`다. 볼륨 LV가 `-revert-` 이름으로만 남았으면 먼저 이름을 되돌려
    /// 볼륨으로 지운다([`volume_lvs`](Self::volume_lvs): 명령이 돌고 있거나 여럿이면 지우지 않는다).
    /// 볼륨 LV가 없어도 그 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 만들기) LV가 곧 생길 수
    /// 있으므로 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lvs = self.volume_lvs(vg, id).await?;
        let found = lvs.iter().any(|lv| lv.name == volume_lv(id));
        if !found && self.ops.changing(vg, &volume_lv(id)) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        if found {
            self.ops.remove(vg, &volume_lv(id)).await?;
        }
        let (snapshots, reverts) = (snapshot_prefix(id), revert_prefix(id));
        for lv in lvs
            .iter()
            .filter(|lv| lv.name.starts_with(&snapshots) || lv.name.starts_with(&reverts))
        {
            self.ops.remove(vg, &lv.name).await?;
        }
        if !found {
            return Err(DriverError::VolumeNotFound(id));
        }
        Ok(())
    }

    /// 비활성인 볼륨 LV를 활성화한다.
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        if !self.existing(vg, id).await?.is_active() {
            self.ops.set_active(vg, &volume_lv(id), true).await?;
        }
        Ok(())
    }

    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = self.existing(vg, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: lv.size_bytes,
            allocated_bytes: lv.used_bytes(),
        })
    }

    /// thin pool의 크기와 남은 데이터 공간
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let (vg, thinpool) = Self::target(pool, ImageFormat::Raw)?;
        let lv = self.thin_pool(vg, thinpool).await?;
        Ok(Capacity {
            total_bytes: lv.size_bytes,
            available_bytes: lv.size_bytes - lv.used_bytes(),
        })
    }

    /// thin pool이 새 블록을 0으로 채우는지([`LvInfo::zeroes_new_blocks`])
    async fn target_reads_zero(&self, pool: &StoragePool) -> Result<bool, DriverError> {
        let (vg, thinpool) = Self::target(pool, ImageFormat::Raw)?;
        Ok(self.thin_pool(vg, thinpool).await?.zeroes_new_blocks())
    }

    /// 볼륨이 raw이므로 thin 스냅샷으로 스냅샷을 둔다.
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
        let (vg, _) = Self::target(pool, format)?;
        let name = Self::snapshot_lv(id, tag)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        if lvs.iter().any(|lv| lv.name == name) {
            return Err(DriverError::InvalidSpec(format!(
                "snapshot {tag} already exists on volume {id}"
            )));
        }
        self.ops.snapshot(vg, &volume_lv(id), &name).await
    }

    /// 볼륨 LV를 `opl-{id}-revert-{tag}`로 옮기고, 스냅샷 LV의 thin 스냅샷을 새 볼륨 LV로 만들어
    /// 활성화하고 활성화 건너뛰기 표시를 지운 뒤 옮긴 옛 LV를 지운다. 새 LV를 만들거나 활성화하지
    /// 못하면 옛 LV를 되돌린다. 볼륨 LV가 `-revert-` 이름으로만 남았으면 먼저 되돌리고
    /// ([`volume_lvs`](Self::volume_lvs)), 앞서 실패한 되돌리기가 남긴 `-revert-` LV는 먼저 지운다
    /// (볼륨 LV는 있다). 단계가 시간을 넘겼으면 다시 보고 끝났으면 이어서 한다.
    async fn snapshot_revert(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let snapshot = self.existing_snapshot(vg, id, tag).await?.name;
        let (volume, old) = (volume_lv(id), format!("{}{tag}", revert_prefix(id)));
        // 볼륨 LV가 있으므로 남은 `-revert-` LV는 모두 옛것이다. 남겨 두면 다음에 볼륨 LV가 없을
        // 때 어느 것이 볼륨인지 모른다.
        for lv in self.ops.list(vg).await? {
            if lv.name.starts_with(&revert_prefix(id)) {
                self.ops.remove(vg, &lv.name).await?;
            }
        }
        if let Err(e) = self.ops.rename(vg, &volume, &old).await
            && !self.done_after(&e, vg, &old, false).await
        {
            return Err(e);
        }
        if let Err(e) = self.ops.snapshot(vg, &snapshot, &volume).await
            && !self.done_after(&e, vg, &volume, false).await
        {
            return Err(self.restore_name(vg, id, &old, e).await);
        }
        if let Err(e) = self.ops.set_active(vg, &volume, true).await
            && !self.done_after(&e, vg, &volume, true).await
        {
            if let Err(del) = self.ops.remove(vg, &volume).await {
                return Err(DriverError::Lvm(format!(
                    "revert of volume {id} failed ({e}); the old volume is left as {vg}/{old} \
                     and the new one as {vg}/{volume}: {del}"
                )));
            }
            return Err(self.restore_name(vg, id, &old, e).await);
        }
        // 스냅샷으로 만든 LV에 붙은 표시를 지워 노드를 다시 켜면 다른 LV처럼 활성화되게 한다.
        // 지우지 못해도 VM을 시작할 때 표시와 상관없이 활성화한다(`-K`).
        if let Err(e) = self.ops.clear_skip(vg, &volume).await {
            tracing::warn!(volume = %id, error = %e, "cannot clear activation skip of reverted volume");
        }
        // 되돌리기는 끝났다. 옛 LV가 남으면 공간만 차지한다.
        if let Err(e) = self.ops.remove(vg, &old).await {
            tracing::warn!(volume = %id, lv = %old, error = %e, "cannot remove reverted volume");
        }
        Ok(())
    }

    /// VM이 실행 중이어도 지울 수 있다(QEMU는 스냅샷 LV를 쓰지 않는다). 스냅샷 LV가 없으면 지운
    /// 것으로 본다. 다만 그 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) LV가 곧 생길
    /// 수 있으므로 일시 오류다.
    async fn snapshot_delete(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: &str,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let name = Self::snapshot_lv(id, tag)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the snapshot",
            ));
        }
        if self.find(vg, &name).await?.is_some() {
            self.ops.remove(vg, &name).await?;
        }
        Ok(())
    }

    async fn snapshot_tags(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lvs = self.volume_lvs(vg, id).await?;
        if !lvs.iter().any(|lv| lv.name == volume_lv(id)) {
            return Err(DriverError::VolumeNotFound(id));
        }
        let prefix = snapshot_prefix(id);
        Ok(lvs
            .iter()
            .filter_map(|lv| lv.name.strip_prefix(&prefix).map(str::to_string))
            .collect())
    }

    /// VM이 실행 중이어도 늘릴 수 있다(그 뒤 QEMU에 새 크기를 알리는 것은 호출자의 일이다).
    /// LV가 이미 그 크기 이상이면(앞선 시도가 늘렸다) 아무것도 하지 않는다.
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        if self.existing(vg, id).await?.size_bytes >= size_bytes {
            return Ok(());
        }
        self.ops.extend(vg, &volume_lv(id), size_bytes).await
    }

    /// 볼륨 LV(이미 쓰고 있을 수 있으므로 내놓을 때 비활성화하지 않는다) 또는 활성화한 스냅샷 LV
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        let lv = match tag {
            Some(tag) => self.existing_snapshot(vg, id, tag).await?,
            None => self.existing(vg, id).await?,
        };
        if !lv.is_active() {
            self.ops.set_active(vg, &lv.name, true).await?;
        }
        Ok(VolumeSource {
            path: device_node(&self.dev, vg, &lv.name)?,
            format,
            snapshot: None,
            size_bytes: lv.size_bytes,
            rbd: None,
        })
    }

    /// 스냅샷 LV를 다시 비활성화한다.
    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        let (vg, _) = Self::target(pool, format)?;
        match tag {
            Some(tag) => {
                let name = Self::snapshot_lv(id, tag)?;
                self.ops.set_active(vg, &name, false).await
            }
            None => Ok(()),
        }
    }

    /// 원본(스냅샷이면 그 LV)의 크기로 대상 볼륨 LV를 만들고 원본을 써 넣는다.
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
        let (vg, thinpool) = Self::target(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        let guard = self.source_guard(src_pool, src_id, format, tag)?;
        let copied = async {
            let dst = self
                .create_lv(vg, thinpool, dst_id, source.size_bytes)
                .await?;
            let from = convert_from(&source, force_share, Some(dst_format))?;
            self.fill(vg, dst_id, &dst, &from).await
        };
        let copied = copied.await;
        guard.release().await;
        copied
    }
}

/// 공유 LVM 풀은 raw 볼륨만 담는다.
pub(super) fn require_shared_raw(format: ImageFormat) -> Result<(), DriverError> {
    match format {
        ImageFormat::Raw => Ok(()),
        ImageFormat::Qcow2 => Err(DriverError::InvalidSpec(
            "lvm-shared pools hold raw volumes".into(),
        )),
    }
}

/// 공유 LVM 볼륨의 크기 단위(1 MiB)
const SHARED_SIZE_UNIT: u64 = 1 << 20;

/// 늘린 뒤 아직 0으로 채우지 않은 부분의 시작(바이트)을 LV에 적는 태그의 앞부분
const ZERO_FROM_TAG: &str = "opl-zero-from-";

/// 이 노드가 LV의 남은 부분을 0으로 채우는 중일 때의 일시 오류(`Timeout`)에 담는 말
const LV_ZEROING: &str = "the lv to be zeroed";

/// 다른 노드가 켜 둔 LV를 바꾸거나 켜려 했을 때의 일시 오류(`Timeout`)에 담는 말
const LV_HELD: &str = "another node to stop using the lv";

/// 다른 노드가 공유 VG의 잠금을 쥐고 있을 때의 일시 오류(`Timeout`)에 담는 말
const VG_HELD: &str = "another node to release the shared vg lock";

/// 다른 노드가 LV를 켜 두었다는 lvmlockd의 말("LV locked by other host")
const LV_HELD_BY: &str = "locked by other host";

/// 다른 노드가 VG 잠금을 쥐고 있다는 lvmlockd의 말("VG ... lock failed: held by other host")
const VG_HELD_BY: &str = "held by other host";

/// 공유 VG에서 다른 노드가 잠금을 쥐어 실패한 LVM 명령은 일시 오류로 바꾼다. 이 말이 든 줄은 앞에
/// 경고 줄("Retrying ex lock …")이 있어도 오류에 담긴다([`LVM`]).
fn held_elsewhere(e: DriverError) -> DriverError {
    match e {
        DriverError::Lvm(msg) if msg.contains(LV_HELD_BY) => DriverError::Timeout(LV_HELD),
        DriverError::Lvm(msg) if msg.contains(VG_HELD_BY) => DriverError::Timeout(VG_HELD),
        e => e,
    }
}

fn no_snapshots() -> DriverError {
    DriverError::InvalidSpec("lvm-shared pools have no snapshots".into())
}

/// 공유 LVM 풀(`lvm-shared`)의 볼륨. 볼륨은 공유 VG의 두꺼운 LV이고, 쓰는 동안만 이 노드에서
/// 배타 활성화한다(VM 디스크면 [`activate_volume`](Storage::activate_volume)부터
/// [`deactivate_volume`](Storage::deactivate_volume)까지). 스냅샷은 없다.
pub struct LvmSharedStorage {
    qemu_img: PathBuf,
    ops: Arc<dyn LvmOps>,
    /// LV 장치 노드가 놓이는 디렉터리([`DEV`]). 테스트는 바꾼다.
    dev: PathBuf,
    /// 이 노드에서 늘린 부분을 0으로 채우는 중인 LV(`{vg}/{lv}`, [`Zeroing`])
    zeroing: Mutex<BTreeSet<String>>,
}

/// LV를 0으로 채우는 중이라는 표시. 버려지면 지운다.
struct Zeroing<'a> {
    set: &'a Mutex<BTreeSet<String>>,
    lv: String,
}

impl<'a> Zeroing<'a> {
    /// 이미 채우는 중이면 일시 오류다.
    fn begin(set: &'a Mutex<BTreeSet<String>>, vg: &str, name: &str) -> Result<Self, DriverError> {
        let lv = format!("{vg}/{name}");
        if !set.lock().unwrap().insert(lv.clone()) {
            return Err(DriverError::Timeout(LV_ZEROING));
        }
        Ok(Self { set, lv })
    }
}

impl Drop for Zeroing<'_> {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.lv);
    }
}

impl LvmSharedStorage {
    pub fn new(qemu_img: impl Into<PathBuf>, ops: Arc<dyn LvmOps>) -> Self {
        Self {
            qemu_img: qemu_img.into(),
            ops,
            dev: PathBuf::from(DEV),
            zeroing: Mutex::default(),
        }
    }

    /// 늘린 뒤 아직 0으로 채우지 않은 부분의 시작(태그 `opl-zero-from-{바이트}`)
    async fn pending_zero(&self, vg: &str, name: &str) -> Result<Option<u64>, DriverError> {
        Ok(self
            .ops
            .tags(vg, name)
            .await?
            .iter()
            .find_map(|t| t.strip_prefix(ZERO_FROM_TAG)?.parse::<u64>().ok()))
    }

    /// 남은 부분(태그의 위치부터 끝까지)을 0으로 채우고 태그를 뗀다. LV는 이 노드에서 켜져 있고
    /// [`Zeroing`]을 쥐고 있어야 한다. 태그의 위치가 LV 끝이면(늘리기 전) 채울 것이 없으므로 태그를
    /// 그대로 둔다(늘린 뒤 채운다).
    async fn zero_pending(&self, vg: &str, id: Uuid) -> Result<(), DriverError> {
        let name = volume_lv(id);
        let Some(from) = self.pending_zero(vg, &name).await? else {
            return Ok(());
        };
        if from >= self.existing(vg, id).await?.size_bytes {
            return Ok(());
        }
        self.ops.zero(vg, &name, from).await?;
        self.ops
            .set_tag(vg, &name, &format!("{ZERO_FROM_TAG}{from}"), false)
            .await
            .map_err(held_elsewhere)
    }

    /// 꺼져 있으면 배타 활성화하고 크기 늘리기가 남긴 0 채우기(태그)를 마친 뒤 LV를 돌려준다. 켜 둔
    /// LV의 장치를 내주기 전에(VM 시작, 복제·복사 원본) 부른다. 남은 부분에는 지운 볼륨의 옛 데이터가
    /// 있을 수 있고, 장치를 쓰기 시작한 뒤에 채우면 쓴 데이터를 지운다. [`Zeroing`]을 잡은 뒤 LV를
    /// 읽으므로 그동안 이 노드의 크기 늘리기가 LV를 켜거나 끄지 않는다. 이 노드가 그 LV를 채우는
    /// 중이거나(크기 늘리기) 그 LV를 바꾸는 명령이 도는 중이면 LV를 건드리지 않고 일시 오류다. 마치지
    /// 못하면 자기가 켠 LV만 끈다.
    async fn activate_ready(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        if self.ops.changing(vg, &name) {
            return Err(DriverError::Timeout(
                "an earlier lvm command still running on the volume",
            ));
        }
        let lv = self.existing(vg, id).await?;
        if !lv.is_active() {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
        }
        let done = self.zero_pending(vg, id).await;
        if done.is_err() && !lv.is_active() {
            self.deactivate_quietly(vg, id).await;
        }
        done.map(|()| lv)
    }

    /// 풀의 VG. raw가 아니면 `InvalidSpec`.
    fn target(pool: &StoragePool, format: ImageFormat) -> Result<&str, DriverError> {
        require_shared_raw(format)?;
        match &pool.spec.backend {
            PoolBackend::LvmShared { vg, .. } => Ok(vg),
            PoolBackend::Dir { .. }
            | PoolBackend::Nfs { .. }
            | PoolBackend::LvmThin { .. }
            | PoolBackend::Zfs { .. }
            | PoolBackend::Rbd { .. } => {
                Err(DriverError::InvalidSpec("not an lvm-shared pool".into()))
            }
        }
    }

    /// 볼륨 LV. 없으면 `VolumeNotFound`.
    async fn existing(&self, vg: &str, id: Uuid) -> Result<LvInfo, DriverError> {
        self.ops
            .list(vg)
            .await?
            .into_iter()
            .find(|lv| lv.name == volume_lv(id))
            .ok_or(DriverError::VolumeNotFound(id))
    }

    /// 볼륨 LV를 만들어 배타 활성화하고 0으로 채운다(켜 둔 채 돌려준다). 크기는 1 MiB 배수로
    /// 올린다. 켜거나 채우지 못하면 만든 LV를 지운다.
    async fn create_lv(&self, vg: &str, id: Uuid, size_bytes: u64) -> Result<PathBuf, DriverError> {
        let name = volume_lv(id);
        if self.ops.list(vg).await?.iter().any(|lv| lv.name == name) {
            return Err(DriverError::VolumeExists(id));
        }
        let size = size_bytes.div_ceil(SHARED_SIZE_UNIT) * SHARED_SIZE_UNIT;
        self.ops
            .create_linear(vg, &name, size)
            .await
            .map_err(held_elsewhere)?;
        let zeroed = async {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
            self.ops.zero(vg, &name, 0).await?;
            device_node(&self.dev, vg, &name)
        };
        match zeroed.await {
            Ok(path) => Ok(path),
            Err(e) => {
                self.remove_partial(vg, id).await;
                Err(e)
            }
        }
    }

    /// 만들다 만 볼륨 LV를 지운다. 지우지 못하면 로그만 남긴다.
    async fn remove_partial(&self, vg: &str, id: Uuid) {
        if let Err(del) = self.ops.remove(vg, &volume_lv(id)).await {
            tracing::warn!(volume = %id, error = %del, "cannot remove partial volume");
        }
    }

    /// 볼륨 LV를 끈다. 끄지 못하면 로그만 남긴다(켜진 채 남은 LV는 opld가 뒤에 정리한다).
    async fn deactivate_quietly(&self, vg: &str, id: Uuid) {
        if let Err(e) = self.ops.set_active(vg, &volume_lv(id), false).await {
            tracing::warn!(volume = %id, error = %e, "cannot deactivate volume");
        }
    }

    /// 새 볼륨 LV를 만들어(0으로 채운다) `qemu-img convert -n --target-is-zero`로 써 넣은 뒤 끈다.
    /// 쓰다 실패하면 볼륨 LV를 지운다.
    async fn fill(
        &self,
        vg: &str,
        id: Uuid,
        size_bytes: u64,
        from: &[String],
    ) -> Result<PathBuf, DriverError> {
        let path = self.create_lv(vg, id, size_bytes).await?;
        let written = async {
            let mut args = vec!["convert", "-q", "-n", "--target-is-zero"];
            args.extend(from.iter().map(String::as_str));
            args.push(path_str(&path)?);
            run_qemu_img(&self.qemu_img, &args).await
        };
        if let Err(e) = written.await {
            self.remove_partial(vg, id).await;
            return Err(e);
        }
        self.deactivate_quietly(vg, id).await;
        Ok(path)
    }
}

#[async_trait]
impl Storage for LvmSharedStorage {
    /// 만들어 0으로 채운 뒤 끈다. 돌려주는 장치 경로는 켜야 생긴다.
    async fn create_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        size_bytes: u64,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let vg = Self::target(pool, format)?;
        self.create_lv(vg, id, size_bytes).await?;
        self.deactivate_quietly(vg, id).await;
        Ok(device_path(&self.dev, vg, &volume_lv(id)))
    }

    /// 원본의 가상 크기로 볼륨 LV를 만들고 raw로 변환해 써 넣는다.
    async fn import_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        src: &Path,
        src_format: SourceFormat,
        format: ImageFormat,
    ) -> Result<PathBuf, DriverError> {
        let vg = Self::target(pool, format)?;
        require_absolute(src)?;
        let src_str = path_str(src)?;
        let size = source_virtual_size(&self.qemu_img, src, src_format).await?;
        let from = ["-f", src_fmt_name(src_format), "-O", "raw", src_str].map(String::from);
        self.fill(vg, id, size, &from).await
    }

    /// 올린 파일만 본다(풀과 상관없다).
    async fn check_import_source(
        &self,
        src: &Path,
        src_format: SourceFormat,
    ) -> Result<(), DriverError> {
        check_image(&self.qemu_img, src, src_format).await
    }

    /// 다른 노드가 켜 두었으면 lvmlockd가 거절하므로 일시 오류다. 볼륨 LV가 없어도 그 LV를 바꾸는
    /// 명령이 아직 돌고 있으면(시간을 넘긴 만들기) LV가 곧 생길 수 있으므로 일시 오류다.
    async fn delete_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        match self.existing(vg, id).await {
            Err(DriverError::VolumeNotFound(_)) if self.ops.changing(vg, &volume_lv(id)) => Err(
                DriverError::Timeout("an earlier lvm command still running on the volume"),
            ),
            Err(e) => Err(e),
            Ok(_) => self
                .ops
                .remove(vg, &volume_lv(id))
                .await
                .map_err(held_elsewhere),
        }
    }

    /// 이 노드에서 꺼져 있으면 배타 활성화한다. 다른 노드가 켜 두었으면 일시 오류다. 크기 늘리기가
    /// 남긴 0 채우기를 마친 뒤 돌아온다([`activate_ready`](Self::activate_ready)).
    async fn activate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        self.activate_ready(vg, id).await.map(drop)
    }

    /// 이 노드에서 켜져 있으면 끈다. 볼륨이 없거나 이미 꺼져 있으면 할 일이 없다. 이 노드가 0으로
    /// 채우는 중인 LV([`Zeroing`])는 끄지 않고 성공이다(켜진 채 남으면 남은 활성화 정리가 끈다).
    /// 끄는 동안 표시를 쥐어 그사이 크기 늘리기가 LV를 켜지 않게 한다.
    async fn deactivate_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let Ok(_zeroing) = Zeroing::begin(&self.zeroing, vg, &volume_lv(id)) else {
            tracing::info!(vg, lv = %volume_lv(id), "lv is being zeroed; leaving it active for the idle sweep");
            return Ok(());
        };
        match self.existing(vg, id).await {
            Ok(lv) if lv.is_active() => self.ops.set_active(vg, &volume_lv(id), false).await,
            Ok(_) | Err(DriverError::VolumeNotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`). 다른 노드가 켠 LV는 이 노드에서 꺼져 보인다.
    async fn active_volumes(&self, pool: &StoragePool) -> Result<Vec<Uuid>, DriverError> {
        let vg = Self::target(pool, ImageFormat::Raw)?;
        Ok(self
            .ops
            .list(vg)
            .await?
            .iter()
            .filter(|lv| lv.is_active())
            .filter_map(|lv| {
                let id = lv.name.strip_prefix("opl-")?.parse::<Uuid>().ok()?;
                (lv.name == volume_lv(id)).then_some(id)
            })
            .collect())
    }

    /// 두꺼운 LV이므로 크기만큼 쓴다.
    async fn volume_info(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
    ) -> Result<VolumeInfo, DriverError> {
        let vg = Self::target(pool, format)?;
        let lv = self.existing(vg, id).await?;
        Ok(VolumeInfo {
            virtual_bytes: lv.size_bytes,
            allocated_bytes: lv.size_bytes,
        })
    }

    /// VG의 크기와 남은 공간
    async fn capacity(&self, pool: &StoragePool) -> Result<Capacity, DriverError> {
        let vg = Self::target(pool, ImageFormat::Raw)?;
        let info = self.ops.vg_info(vg).await?;
        Ok(Capacity {
            total_bytes: info.size_bytes,
            available_bytes: info.free_bytes,
        })
    }

    /// 새 볼륨은 만들 때 0으로 채운다.
    async fn target_reads_zero(&self, _pool: &StoragePool) -> Result<bool, DriverError> {
        Ok(true)
    }

    fn supports_snapshots(&self, _: &StoragePool, _: ImageFormat) -> bool {
        false
    }

    async fn snapshot_create(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_revert(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_delete(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
        _: &str,
    ) -> Result<(), DriverError> {
        Err(no_snapshots())
    }

    async fn snapshot_tags(
        &self,
        _: &StoragePool,
        _: Uuid,
        _: ImageFormat,
    ) -> Result<Vec<String>, DriverError> {
        Err(no_snapshots())
    }

    /// 늘린 뒤 새 부분(늘리기 전 크기부터 끝까지)을 0으로 채운다. 새 익스텐트에는 지운 볼륨의 옛
    /// 데이터가 남아 있을 수 있다. 꺼져 있으면 배타 활성화해 채운 뒤 끄고, 이 노드에서 켜져 있으면
    /// (실행 중인 VM) 켠 채로 둔다. 다른 노드가 켜 두었으면 일시 오류다.
    ///
    /// 늘리기 전에 늘리기 전 크기를 태그(`opl-zero-from-{바이트}`)로 붙이고 다 채운 뒤 뗀다. 채우기가
    /// 실패하면 오류이고 태그가 남으므로, 다시 하면(LV가 이미 그 크기여도) 남은 부분을 다시 채운다.
    /// 그 전에 VM 시작이나 복제 원본이 LV를 켜면 그쪽이 마저 채운다. 태그가 없고 LV가 이미 그 크기
    /// 이상이면 할 일이 없다. LV를 읽기 전부터 다 채울 때까지 [`Zeroing`]을 쥐어, 그동안 이 노드의
    /// VM 시작·복제 원본은 일시 오류다(읽은 활성 여부가 끝까지 맞다).
    async fn resize_volume(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        size_bytes: u64,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        let name = volume_lv(id);
        let _zeroing = Zeroing::begin(&self.zeroing, vg, &name)?;
        let lv = self.existing(vg, id).await?;
        let marked = self.pending_zero(vg, &name).await?;
        let grow = lv.size_bytes < size_bytes;
        if !grow && marked.is_none() {
            return Ok(());
        }
        if grow {
            if marked.is_none() {
                let tag = format!("{ZERO_FROM_TAG}{}", lv.size_bytes);
                self.ops
                    .set_tag(vg, &name, &tag, true)
                    .await
                    .map_err(held_elsewhere)?;
            }
            self.ops
                .extend(vg, &name, size_bytes)
                .await
                .map_err(held_elsewhere)?;
        }
        if !lv.is_active() {
            self.ops
                .activate_exclusive(vg, &name)
                .await
                .map_err(held_elsewhere)?;
        }
        let zeroed = self.zero_pending(vg, id).await;
        if !lv.is_active() {
            self.deactivate_quietly(vg, id).await;
        }
        zeroed
    }

    /// 볼륨 LV를 배타 활성화하고 남은 0 채우기를 마친 뒤 읽게 한다. 스냅샷이 없으므로 `tag`는
    /// `InvalidSpec`이다.
    async fn volume_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<VolumeSource, DriverError> {
        let vg = Self::target(pool, format)?;
        if tag.is_some() {
            return Err(no_snapshots());
        }
        let lv = self.activate_ready(vg, id).await?;
        Ok(VolumeSource {
            path: device_node(&self.dev, vg, &lv.name)?,
            format,
            snapshot: None,
            size_bytes: lv.size_bytes,
            rbd: None,
        })
    }

    /// 볼륨 LV를 끈다.
    async fn release_source(
        &self,
        pool: &StoragePool,
        id: Uuid,
        format: ImageFormat,
        tag: Option<&str>,
    ) -> Result<(), DriverError> {
        let vg = Self::target(pool, format)?;
        if tag.is_some() {
            return Err(no_snapshots());
        }
        self.ops.set_active(vg, &volume_lv(id), false).await
    }

    /// 원본 볼륨을 켜서 그 크기로 대상 볼륨 LV를 만들고 써 넣은 뒤 둘 다 끈다.
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
        let vg = Self::target(dst_pool, dst_format)?;
        let source = self.volume_source(src_pool, src_id, format, tag).await?;
        // 읽는 동안 취소돼도 원본 LV를 끈다.
        let guard = SourceGuard {
            ops: self.ops.clone(),
            vg: Self::target(src_pool, format)?.to_string(),
            lv: Some(volume_lv(src_id)),
        };
        let copied = async {
            let from = convert_from(&source, force_share, Some(dst_format))?;
            self.fill(vg, dst_id, source.size_bytes, &from).await
        };
        let copied = copied.await;
        guard.release().await;
        copied.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use opl_types::resource::Resource;
    use opl_types::storage::StoragePoolSpec;

    use super::*;

    #[derive(Debug, Clone)]
    struct FakeLv {
        size: u64,
        active: bool,
        /// 활성화 건너뛰기 표시
        skip: bool,
    }

    /// VG `vg0` 하나를 메모리에 두는 LVM. thin pool `tp`와 관계없는 LV `root`가 있다. 바꾸는 호출을
    /// 적고, `fail`에 적은 호출은 실패한다. `stuck`에 적은 호출은 하지 않고, `late`에 적은 이름
    /// 바꾸기·스냅샷은 해 놓고 `Timeout`을 돌려준다(시간을 넘긴 명령). 활성인 LV의 장치 노드를 `dev`
    /// 아래에 둔다(udev처럼). `busy`에 적은 LV는 바꾸는 명령이 아직 돌고 있다고 알린다. `held`에
    /// 적은 LV는 다른 노드가 켜 두었다(공유 VG: 켜기·지우기·늘리기를 lvmlockd가 거절한다).
    /// `on_list`는 다음 `list`가 결과를 만든 뒤 한 번 부른다(읽은 뒤 끼어드는 다른 작업).
    struct FakeLvm {
        lvs: Mutex<BTreeMap<String, FakeLv>>,
        busy: Mutex<Vec<String>>,
        held: Mutex<Vec<String>>,
        /// LV별 태그
        tags: Mutex<BTreeMap<String, Vec<String>>>,
        log: Mutex<Vec<String>>,
        fail: Mutex<Vec<String>>,
        late: Mutex<Vec<String>>,
        stuck: Mutex<Vec<String>>,
        /// thin pool `tp`가 새 블록을 0으로 채우는지(`lv_attr` 여덟째 글자 `z`)
        zero: Mutex<bool>,
        on_list: Mutex<Option<OnList>>,
        dev: tempfile::TempDir,
    }

    type OnList = Box<dyn FnOnce(&mut BTreeMap<String, FakeLv>) + Send>;

    impl Default for FakeLvm {
        fn default() -> Self {
            Self {
                lvs: Mutex::default(),
                busy: Mutex::default(),
                held: Mutex::default(),
                tags: Mutex::default(),
                log: Mutex::default(),
                fail: Mutex::default(),
                late: Mutex::default(),
                stuck: Mutex::default(),
                zero: Mutex::new(true),
                on_list: Mutex::default(),
                dev: tempfile::tempdir().unwrap(),
            }
        }
    }

    impl FakeLvm {
        /// LV `name`의 장치 노드 경로
        fn node(&self, name: &str) -> PathBuf {
            device_path(self.dev.path(), "vg0", name)
        }

        /// 장치 노드를 활성인 LV들에 맞춘다.
        fn sync_nodes(&self, lvs: &BTreeMap<String, FakeLv>) {
            let dir = self.dev.path().join("vg0");
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for name in lvs.iter().filter(|(_, lv)| lv.active).map(|(name, _)| name) {
                std::fs::write(dir.join(name), b"").unwrap();
            }
        }

        fn call(&self, call: String) -> Result<(), DriverError> {
            let failing = self.fail.lock().unwrap().contains(&call);
            let stuck = self.stuck.lock().unwrap().contains(&call);
            self.log.lock().unwrap().push(call.clone());
            if failing {
                return Err(DriverError::Lvm(format!("injected: {call}")));
            }
            if stuck {
                return Err(DriverError::Timeout("lvm command"));
            }
            Ok(())
        }

        fn fail_on(&self, call: impl Into<String>) {
            self.fail.lock().unwrap().push(call.into());
        }

        /// 해 놓은 호출 `call`이 `late`에 있으면 `Timeout`
        fn finish(&self, call: &str) -> Result<(), DriverError> {
            if self.late.lock().unwrap().iter().any(|c| c == call) {
                return Err(DriverError::Timeout("lvm command"));
            }
            Ok(())
        }

        fn taken(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }

        fn names(&self) -> Vec<String> {
            self.lvs.lock().unwrap().keys().cloned().collect()
        }

        fn lv(&self, name: &str) -> Option<FakeLv> {
            self.lvs.lock().unwrap().get(name).cloned()
        }

        fn missing(name: &str) -> DriverError {
            DriverError::Lvm(format!("Failed to find logical volume \"vg0/{name}\""))
        }

        /// 다른 노드가 켜 둔 LV면 lvmlockd의 거절
        fn check_held(&self, name: &str) -> Result<(), DriverError> {
            if self.held.lock().unwrap().iter().any(|lv| lv == name) {
                return Err(DriverError::Lvm(format!(
                    "lvchange: LV locked by other host: vg0/{name}"
                )));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl LvmOps for FakeLvm {
        async fn list(&self, vg: &str) -> Result<Vec<LvInfo>, DriverError> {
            assert_eq!(vg, "vg0");
            let mut out = vec![
                LvInfo {
                    name: "root".into(),
                    attr: "-wi-ao----".into(),
                    size_bytes: 1 << 30,
                    data_percent: None,
                },
                LvInfo {
                    name: "tp".into(),
                    attr: if *self.zero.lock().unwrap() {
                        "twi-aotz--"
                    } else {
                        "twi-aot---"
                    }
                    .into(),
                    size_bytes: 8 << 30,
                    data_percent: Some(25.0),
                },
            ];
            for (name, lv) in self.lvs.lock().unwrap().iter() {
                out.push(LvInfo {
                    name: name.clone(),
                    attr: if lv.active {
                        "Vwi-a-tz--"
                    } else {
                        "Vwi---tz--"
                    }
                    .into(),
                    size_bytes: lv.size,
                    data_percent: lv.active.then_some(50.0),
                });
            }
            let hook = self.on_list.lock().unwrap().take();
            if let Some(hook) = hook {
                hook(&mut self.lvs.lock().unwrap());
            }
            Ok(out)
        }

        async fn create_thin(
            &self,
            vg: &str,
            thinpool: &str,
            name: &str,
            size_bytes: u64,
        ) -> Result<(), DriverError> {
            self.call(format!("create {vg}/{thinpool} {name} {size_bytes}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            let lv = FakeLv {
                size: size_bytes,
                active: true,
                skip: false,
            };
            lvs.insert(name.into(), lv);
            self.sync_nodes(&lvs);
            Ok(())
        }

        async fn snapshot(&self, vg: &str, origin: &str, name: &str) -> Result<(), DriverError> {
            let call = format!("snapshot {vg}/{origin} {name}");
            self.call(call.clone())?;
            let mut lvs = self.lvs.lock().unwrap();
            let size = lvs.get(origin).ok_or_else(|| Self::missing(origin))?.size;
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            // thin 스냅샷은 활성화 건너뛰기 표시가 붙어 비활성이다.
            let lv = FakeLv {
                size,
                active: false,
                skip: true,
            };
            lvs.insert(name.into(), lv);
            self.finish(&call)
        }

        async fn remove(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("remove {vg}/{name}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            let removed = lvs.remove(name);
            self.sync_nodes(&lvs);
            removed.map(drop).ok_or_else(|| Self::missing(name))
        }

        async fn extend(&self, vg: &str, name: &str, size_bytes: u64) -> Result<(), DriverError> {
            self.call(format!("extend {vg}/{name} {size_bytes}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            let lv = lvs.get_mut(name).ok_or_else(|| Self::missing(name))?;
            if size_bytes <= lv.size {
                return Err(DriverError::Lvm("New size not larger".into()));
            }
            lv.size = size_bytes;
            Ok(())
        }

        async fn rename(&self, vg: &str, from: &str, to: &str) -> Result<(), DriverError> {
            let call = format!("rename {vg}/{from} {to}");
            self.call(call.clone())?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(to) {
                return Err(DriverError::Lvm(format!("{to} already exists")));
            }
            let lv = lvs.remove(from).ok_or_else(|| Self::missing(from))?;
            lvs.insert(to.into(), lv);
            self.sync_nodes(&lvs);
            self.finish(&call)
        }

        async fn set_active(&self, vg: &str, name: &str, active: bool) -> Result<(), DriverError> {
            let op = if active { "activate" } else { "deactivate" };
            self.call(format!("{op} {vg}/{name}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.active = active;
            self.sync_nodes(&lvs);
            Ok(())
        }

        async fn clear_skip(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("unskip {vg}/{name}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.skip = false;
            Ok(())
        }

        fn changing(&self, vg: &str, name: &str) -> bool {
            assert_eq!(vg, "vg0");
            self.busy.lock().unwrap().iter().any(|lv| lv == name)
        }

        async fn create_linear(
            &self,
            vg: &str,
            name: &str,
            size_bytes: u64,
        ) -> Result<(), DriverError> {
            self.call(format!("create-linear {vg} {name} {size_bytes}"))?;
            let mut lvs = self.lvs.lock().unwrap();
            if lvs.contains_key(name) {
                return Err(DriverError::Lvm(format!("{name} already exists")));
            }
            let lv = FakeLv {
                size: size_bytes,
                active: false,
                skip: false,
            };
            lvs.insert(name.into(), lv);
            Ok(())
        }

        async fn activate_exclusive(&self, vg: &str, name: &str) -> Result<(), DriverError> {
            self.call(format!("activate-ex {vg}/{name}"))?;
            self.check_held(name)?;
            let mut lvs = self.lvs.lock().unwrap();
            lvs.get_mut(name).ok_or_else(|| Self::missing(name))?.active = true;
            self.sync_nodes(&lvs);
            Ok(())
        }

        /// 켜 둔 LV만 채울 수 있다(장치 노드가 있어야 한다).
        async fn zero(&self, vg: &str, name: &str, offset: u64) -> Result<(), DriverError> {
            self.call(format!("zero {vg}/{name} {offset}"))?;
            if !self.lv(name).is_some_and(|lv| lv.active) {
                return Err(DriverError::Lvm(format!("blkdiscard: {name} not active")));
            }
            Ok(())
        }

        async fn tags(&self, vg: &str, name: &str) -> Result<Vec<String>, DriverError> {
            assert_eq!(vg, "vg0");
            self.lv(name).ok_or_else(|| Self::missing(name))?;
            Ok(self
                .tags
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .unwrap_or_default())
        }

        async fn set_tag(
            &self,
            vg: &str,
            name: &str,
            tag: &str,
            on: bool,
        ) -> Result<(), DriverError> {
            let op = if on { "addtag" } else { "deltag" };
            self.call(format!("{op} {vg}/{name} {tag}"))?;
            self.lv(name).ok_or_else(|| Self::missing(name))?;
            let mut tags = self.tags.lock().unwrap();
            let tags = tags.entry(name.into()).or_default();
            tags.retain(|t| t != tag);
            if on {
                tags.push(tag.into());
            }
            Ok(())
        }

        async fn vg_info(&self, vg: &str) -> Result<VgInfo, DriverError> {
            assert_eq!(vg, "vg0");
            Ok(VgInfo {
                name: "vg0".into(),
                lock_type: "sanlock".into(),
                size_bytes: 10 << 30,
                free_bytes: 4 << 30,
            })
        }
    }

    fn lvm_pool() -> StoragePool {
        Resource::new(
            "vg",
            StoragePoolSpec {
                backend: PoolBackend::LvmThin {
                    vg: "vg0".into(),
                    thinpool: "tp".into(),
                },
                nodes: vec![],
                shared: false,
            },
        )
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

    fn setup(qemu_img: impl Into<PathBuf>) -> (Arc<FakeLvm>, LvmThinStorage) {
        let lvm = Arc::new(FakeLvm::default());
        let storage = LvmThinStorage {
            dev: lvm.dev.path().to_path_buf(),
            ..LvmThinStorage::new(qemu_img, lvm.clone())
        };
        (lvm, storage)
    }

    fn invalid(e: DriverError, want: &str) {
        match e {
            DriverError::InvalidSpec(msg) => assert!(msg.contains(want), "{msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    const RAW: ImageFormat = ImageFormat::Raw;

    #[tokio::test]
    async fn creates_raw_thin_volumes() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        // 크기는 512 배수로 올린다.
        let path = storage
            .create_volume(&p, id, (1 << 20) + 1, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&format!("opl-{id}")));
        assert_eq!(lvm.taken(), [format!("create vg0/tp opl-{id} 1049088")]);

        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        let qcow2 = ImageFormat::Qcow2;
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), 1 << 20, qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
        assert_eq!(lvm.taken(), Vec::<String>::new());
    }

    /// 원본의 가상 크기로 LV를 만들고 raw로 변환해 써 넣는다. 실패하면 LV를 지운다.
    #[tokio::test]
    async fn imports_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        let node = lvm.node(&format!("opl-{id}"));
        let dev = node.to_str().unwrap();
        assert_eq!(path, node);
        assert_eq!(lvm.taken(), [format!("create vg0/tp opl-{id} 1024")]);
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
                dev,
            ]
        );

        let (lvm, storage) = setup(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{id} 1024"),
                format!("remove vg0/opl-{id}")
            ]
        );
        assert_eq!(lvm.names(), Vec::<String>::new());
        // 상대 경로는 qemu-img가 프로토콜로 읽을 수 있다.
        invalid(
            storage
                .import_volume(&p, id, "x.vmdk".as_ref(), SourceFormat::Vmdk, RAW)
                .await
                .unwrap_err(),
            "absolute",
        );
    }

    /// 볼륨을 지우면 볼륨 LV를 먼저 지우고 그 스냅샷 LV와 실패한 되돌리기가 남긴 LV도 지운다. 다른
    /// 볼륨의 LV는 그대로다.
    #[tokio::test]
    async fn delete_removes_volume_with_its_snapshots() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let p = lvm_pool();
        let (id, other) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [id, other] {
            storage.create_volume(&p, v, 1 << 20, RAW).await.unwrap();
            storage.snapshot_create(&p, v, RAW, "a").await.unwrap();
        }
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        lvm.snapshot("vg0", &format!("opl-{id}"), &format!("opl-{id}-revert-a"))
            .await
            .unwrap();
        lvm.taken();

        // 볼륨 LV를 지우지 못하면(쓰는 중) 스냅샷은 그대로다.
        lvm.fail_on(format!("remove vg0/opl-{id}"));
        let names = lvm.names();
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{id}")]);
        assert_eq!(lvm.names(), names);
        lvm.fail.lock().unwrap().clear();

        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("remove vg0/opl-{id}"),
                format!("remove vg0/opl-{id}-revert-a"),
                format!("remove vg0/opl-{id}-s-a"),
                format!("remove vg0/opl-{id}-s-b"),
            ]
        );
        assert_eq!(
            lvm.names(),
            [format!("opl-{other}"), format!("opl-{other}-s-a")]
        );
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeNotFound(v) if v == id),
            "{err:?}"
        );

        // 볼륨 LV를 지운 뒤 멈췄으면 다시 지울 때 남은 스냅샷 LV를 지운다.
        lvm.fail_on(format!("remove vg0/opl-{other}-s-a"));
        storage.delete_volume(&p, other, RAW).await.unwrap_err();
        lvm.fail.lock().unwrap().clear();
        assert_eq!(lvm.names(), [format!("opl-{other}-s-a")]);
        lvm.taken();
        let err = storage.delete_volume(&p, other, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{other}-s-a")]);
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn reports_volume_usage_and_thin_pool_capacity() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.volume_info(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 1 << 20,
                allocated_bytes: 1 << 19,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 8 << 30,
                available_bytes: 6 << 30,
            }
        );
        // thin pool이 없거나 thin pool이 아니다.
        let mut root = lvm_pool();
        root.spec.backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "root".into(),
        };
        let err = storage.capacity(&root).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("vg0/root")),
            "{err:?}"
        );
        assert_eq!(lvm.taken().len(), 1);
    }

    #[tokio::test]
    async fn activates_only_inactive_volumes() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
        lvm.set_active("vg0", &format!("opl-{id}"), false)
            .await
            .unwrap();
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate vg0/opl-{id}")]);
        assert!(lvm.lv(&format!("opl-{id}")).unwrap().active);
    }

    #[tokio::test]
    async fn resize_extends_the_lv() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(lvm.taken(), [format!("extend vg0/opl-{id} 2097152")]);
        assert_eq!(lvm.lv(&format!("opl-{id}")).unwrap().size, 2 << 20);
        // 이미 그 크기 이상이면(앞선 시도가 늘렸다) lvextend를 부르지 않는다.
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 1 << 20).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
        assert_eq!(lvm.lv(&format!("opl-{id}")).unwrap().size, 2 << 20);
    }

    #[tokio::test]
    async fn snapshots_are_thin_snapshot_lvs() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        assert!(storage.supports_snapshots(&p, RAW));
        assert!(!storage.supports_snapshots(&p, ImageFormat::Qcow2));
        let err = storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();

        storage.snapshot_create(&p, id, RAW, "a").await.unwrap();
        storage.snapshot_create(&p, id, RAW, "b").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("snapshot vg0/opl-{id} opl-{id}-s-a"),
                format!("snapshot vg0/opl-{id} opl-{id}-s-b"),
            ]
        );
        invalid(
            storage.snapshot_create(&p, id, RAW, "a").await.unwrap_err(),
            "already exists",
        );
        invalid(
            storage
                .snapshot_create(&p, id, RAW, "a/b")
                .await
                .unwrap_err(),
            "not a valid lvm name",
        );
        assert_eq!(
            storage.snapshot_tags(&p, id, RAW).await.unwrap(),
            ["a", "b"]
        );

        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        // 없는 태그는 지운 것으로 본다.
        storage.snapshot_delete(&p, id, RAW, "a").await.unwrap();
        assert_eq!(lvm.taken(), [format!("remove vg0/opl-{id}-s-a")]);
        assert_eq!(storage.snapshot_tags(&p, id, RAW).await.unwrap(), ["b"]);
        invalid(
            storage
                .snapshot_tags(&p, id, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "raw volumes",
        );
    }

    /// 그 스냅샷 LV를 바꾸는 명령이 아직 돌고 있으면(시간을 넘긴 스냅샷 등) LV가 아직 없어도
    /// 지우기는 일시 오류다. 명령이 끝나면 LV가 생길 수 있다.
    #[tokio::test]
    async fn snapshot_delete_waits_for_a_running_command() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        *lvm.busy.lock().unwrap() = vec![format!("opl-{id}-s-t")];
        let err = storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        lvm.busy.lock().unwrap().clear();
        storage.snapshot_delete(&p, id, RAW, "t").await.unwrap();
        assert!(lvm.taken().is_empty());
    }

    /// 볼륨 LV가 아직 없어도 그 LV를 바꾸는 명령이 돌고 있으면(시간을 넘긴 만들기) 지우기는 일시
    /// 오류다. `VolumeNotFound`로 자원을 지우면 명령이 끝나 생긴 LV가 남는다.
    #[tokio::test]
    async fn delete_waits_for_a_running_create() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        *lvm.busy.lock().unwrap() = vec![format!("opl-{id}")];
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(err.is_transient());
        assert!(lvm.taken().is_empty());
        lvm.busy.lock().unwrap().clear();
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
    }

    /// 여러 볼륨의 스냅샷은 기본 구현대로 하나씩 만든다.
    #[tokio::test]
    async fn snapshot_many_takes_one_volume_at_a_time() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let p = lvm_pool();
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        for v in [a, b] {
            storage.create_volume(&p, v, 1 << 20, RAW).await.unwrap();
        }
        lvm.taken();
        storage.snapshot_many(&p, &[a, b], RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("snapshot vg0/opl-{a} opl-{a}-s-t"),
                format!("snapshot vg0/opl-{b} opl-{b}-s-t"),
            ]
        );
    }

    /// 볼륨 LV를 옮겨 두고 스냅샷 LV의 스냅샷을 새 볼륨 LV로 만든 뒤 옛 LV를 지운다.
    #[tokio::test]
    async fn revert_replaces_volume_with_a_snapshot_of_the_snapshot() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        lvm.taken();
        invalid(
            storage.snapshot_revert(&p, id, RAW, "x").await.unwrap_err(),
            "not found",
        );

        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        // 스냅샷 때의 크기로 돌아가고, 스냅샷은 남는다. 새 볼륨 LV는 활성화 건너뛰기 표시가 없다.
        let lv = lvm.lv(&volume).unwrap();
        assert!(lv.active);
        assert!(!lv.skip);
        assert_eq!(lv.size, 1 << 20);
        assert_eq!(lvm.names(), [volume.clone(), format!("opl-{id}-s-t")]);

        // 앞서 실패한 되돌리기가 남긴 옛 LV는 먼저 지운다.
        lvm.snapshot("vg0", &volume, &old).await.unwrap();
        lvm.taken();
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("remove vg0/{old}"),
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        assert_eq!(lvm.names(), [volume, format!("opl-{id}-s-t")]);
    }

    /// 새 볼륨 LV를 만들거나 활성화하지 못하면 옛 LV의 이름을 되돌린다.
    #[tokio::test]
    async fn failed_revert_restores_the_volume() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let names = lvm.names();
        lvm.taken();

        lvm.fail_on(format!("snapshot vg0/opl-{id}-s-t {volume}"));
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("rename vg0/{old} {volume}"),
            ]
        );
        assert_eq!(lvm.names(), names);

        lvm.fail.lock().unwrap().clear();
        lvm.fail_on(format!("activate vg0/{volume}"));
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/opl-{id}-s-t {volume}"),
                format!("activate vg0/{volume}"),
                format!("remove vg0/{volume}"),
                format!("rename vg0/{old} {volume}"),
            ]
        );
        assert_eq!(lvm.names(), names);

        // 이름을 되돌리지 못하면 남은 LV를 알린다.
        lvm.fail.lock().unwrap().clear();
        lvm.fail_on(format!("snapshot vg0/opl-{id}-s-t {volume}"));
        lvm.fail_on(format!("rename vg0/{old} {volume}"));
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains(&format!("vg0/{old}"))),
            "{err:?}"
        );
    }

    /// 이름 바꾸기나 새 볼륨 LV 만들기가 시간을 넘겼으면 `lvs`로 다시 보고, 끝났으면 되돌리기를
    /// 이어서 끝낸다.
    #[tokio::test]
    async fn revert_goes_on_after_a_step_that_timed_out_but_finished() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let steps = [
            format!("rename vg0/{volume} {old}"),
            format!("snapshot vg0/opl-{id}-s-t {volume}"),
            format!("activate vg0/{volume}"),
            format!("unskip vg0/{volume}"),
            format!("remove vg0/{old}"),
        ];
        for late in &steps[..2] {
            lvm.taken();
            *lvm.late.lock().unwrap() = vec![late.clone()];
            storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
            assert_eq!(lvm.taken(), steps, "late: {late}");
            assert_eq!(lvm.names(), [volume.clone(), format!("opl-{id}-s-t")]);
            assert!(lvm.lv(&volume).unwrap().active);
        }

        // 시간을 넘긴 이름 바꾸기가 끝나지 않았으면 그 오류로 끝난다(볼륨 LV는 그대로다).
        lvm.late.lock().unwrap().clear();
        lvm.taken();
        *lvm.stuck.lock().unwrap() = vec![steps[0].clone()];
        let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
        assert_eq!(lvm.taken(), steps[..1]);
        assert_eq!(lvm.names(), [volume, format!("opl-{id}-s-t")]);
    }

    /// 시간을 넘긴 이름 바꾸기가 되돌리기가 실패한 뒤에 끝나 볼륨 LV가 `-revert-` 이름으로만
    /// 남았으면, 그 LV를 다루는 명령이 돌고 있지 않을 때 볼륨 LV로 이름을 되돌린 뒤 이어서 한다.
    /// 돌고 있으면 일시 오류이고, `-revert-` LV가 여럿이면 어느 것인지 모르므로 건드리지 않는다.
    #[tokio::test]
    async fn volume_left_under_a_revert_name_is_restored() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        let (volume, old) = (format!("opl-{id}"), format!("opl-{id}-revert-t"));
        let snapshot = format!("opl-{id}-s-t");
        // 되돌리기의 이름 바꾸기가 시간을 넘겨 실패하고, 그 뒤에 끝난다.
        let leave = || async {
            *lvm.stuck.lock().unwrap() = vec![format!("rename vg0/{volume} {old}")];
            let err = storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err();
            assert!(matches!(err, DriverError::Timeout(_)), "{err:?}");
            lvm.stuck.lock().unwrap().clear();
            lvm.rename("vg0", &volume, &old).await.unwrap();
            assert_eq!(lvm.names(), [old.clone(), snapshot.clone()]);
            lvm.taken();
        };

        leave().await;
        // 아직 돌고 있으면 볼륨이 없다고 하지 않고(VM을 끄지 않는다) 잠시 뒤 다시 해 볼 오류다.
        for busy in [&old, &volume] {
            *lvm.busy.lock().unwrap() = vec![busy.clone()];
            let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
            assert!(err.is_transient(), "{busy}: {err:?}");
            let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
            assert!(err.is_transient(), "{busy}: {err:?}");
            assert_eq!(lvm.names(), [old.clone(), snapshot.clone()]);
        }
        assert!(lvm.taken().is_empty());
        lvm.busy.lock().unwrap().clear();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("rename vg0/{old} {volume}")]);
        assert_eq!(lvm.names(), [volume.clone(), snapshot.clone()]);
        let info = storage.volume_info(&p, id, RAW).await.unwrap();
        assert_eq!(info.virtual_bytes, 1 << 20);

        // 되돌리기를 다시 하면 이름을 되돌린 뒤 처음부터 한다.
        leave().await;
        storage.snapshot_revert(&p, id, RAW, "t").await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("rename vg0/{old} {volume}"),
                format!("rename vg0/{volume} {old}"),
                format!("snapshot vg0/{snapshot} {volume}"),
                format!("activate vg0/{volume}"),
                format!("unskip vg0/{volume}"),
                format!("remove vg0/{old}"),
            ]
        );
        assert_eq!(lvm.names(), [volume.clone(), snapshot.clone()]);

        // `-revert-` LV가 여럿이면 어느 것이 볼륨인지 모른다. 지우기도 아무것도 지우지 않는다.
        leave().await;
        let other = format!("opl-{id}-revert-u");
        lvm.snapshot("vg0", &snapshot, &other).await.unwrap();
        lvm.taken();
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains(&old) && m.contains(&other)),
            "{err:?}"
        );
        storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(lvm.taken().is_empty());
        lvm.remove("vg0", &other).await.unwrap();

        // 볼륨을 지우면 `-revert-` LV를 볼륨으로 보고 지운다(볼륨이 없다고 하지 않는다).
        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert!(lvm.names().is_empty());
    }

    /// 스냅샷은 활성화해 읽고 내놓을 때 비활성화한다. 볼륨 자체는 활성화만 한다.
    #[tokio::test]
    async fn volume_source_activates_snapshot_until_released() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage.volume_source(&p, id, RAW, None).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, id, RAW, "t").await.unwrap();
        lvm.taken();

        let snap = format!("opl-{id}-s-t");
        assert_eq!(
            storage.volume_source(&p, id, RAW, Some("t")).await.unwrap(),
            VolumeSource {
                path: lvm.node(&snap),
                format: RAW,
                snapshot: None,
                size_bytes: 1 << 20,
                rbd: None,
            }
        );
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("deactivate vg0/{snap}")
            ]
        );
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("x"))
                .await
                .unwrap_err(),
            "not found",
        );

        // 크기는 읽는 LV의 것이다(스냅샷이면 그때의 크기).
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        let snapshot = storage.volume_source(&p, id, RAW, Some("t")).await.unwrap();
        assert_eq!(snapshot.size_bytes, 1 << 20);
        storage
            .release_source(&p, id, RAW, Some("t"))
            .await
            .unwrap();
        lvm.taken();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.path, lvm.node(&format!("opl-{id}")));
        assert_eq!(source.size_bytes, 2 << 20);
        storage.release_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(lvm.taken(), Vec::<String>::new());
    }

    /// 같은 VG 안 복사: 원본(스냅샷 LV)의 크기로 LV를 만들고 원본을 써 넣는다.
    #[tokio::test]
    async fn copies_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let p = lvm_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        // 스냅샷 뒤에 늘렸어도 스냅샷 때의 크기로 만든다.
        storage.resize_volume(&p, src, RAW, 2 << 20).await.unwrap();
        lvm.taken();

        storage
            .copy_volume(&p, src, RAW, Some("t"), true, &p, dst, RAW)
            .await
            .unwrap();
        let snap = format!("opl-{src}-s-t");
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("create vg0/tp opl-{dst} 1048576"),
                format!("deactivate vg0/{snap}"),
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to) = (lvm.node(&snap), lvm.node(&format!("opl-{dst}")));
        let (from, to) = (from.to_str().unwrap(), to.to_str().unwrap());
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
                from,
                to,
            ]
        );
        // 대상이 이미 있으면 덮어쓰지 않는다. 원본은 내놓는다.
        let err = storage
            .copy_volume(&p, src, RAW, Some("t"), true, &p, dst, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == dst),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("activate vg0/{snap}"),
                format!("deactivate vg0/{snap}")
            ]
        );

        // 쓰다 실패하면 만든 LV를 지운다.
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), true));
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{dst} 1048576"),
                format!("remove vg0/opl-{dst}"),
            ]
        );
        assert_eq!(lvm.names(), [format!("opl-{src}")]);
    }

    /// 장치 노드는 `{dev}/{vg}/{lv}`, 없으면 device-mapper 노드다. 둘 다 없으면 노드가 없다는 오류다.
    #[test]
    fn finds_device_nodes() {
        let dev = tempfile::tempdir().unwrap();
        let (vg, lv) = ("vg-0", "opl-x");
        let path = dev.path().join("vg-0/opl-x");
        let err = device_node(dev.path(), vg, lv).unwrap_err();
        assert!(err.to_string().contains("device node missing"), "{err}");
        assert!(
            matches!(err, DriverError::VolumeNotActive(ref p) if *p == path),
            "{err:?}"
        );
        // device-mapper는 이름 안의 `-`를 `--`로 쓴다.
        let mapper = dev.path().join("mapper/vg--0-opl--x");
        std::fs::create_dir(dev.path().join("mapper")).unwrap();
        std::fs::write(&mapper, b"").unwrap();
        assert_eq!(device_node(dev.path(), vg, lv).unwrap(), mapper);
        std::fs::create_dir(dev.path().join(vg)).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(device_node(dev.path(), vg, lv).unwrap(), path);
    }

    /// 장치 노드가 생기지 않으면 만든 LV를 지운다.
    #[tokio::test]
    async fn create_without_device_node_removes_the_lv() {
        let (lvm, storage) = setup("/nonexistent/qemu-img");
        let storage = LvmThinStorage {
            dev: lvm.dev.path().join("elsewhere"),
            ..storage
        };
        let (p, id) = (lvm_pool(), Uuid::now_v7());
        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotActive(_)), "{err:?}");
        assert_eq!(
            lvm.taken(),
            [
                format!("create vg0/tp opl-{id} 1048576"),
                format!("remove vg0/opl-{id}"),
            ]
        );
    }

    /// LVM 명령은 숫자를 `.`로 쓰고 파일 기술자 경고를 끄며, 실패는 `Lvm` 오류다.
    #[tokio::test]
    async fn lvm_commands_use_c_locale_and_lvm_errors() {
        let long = Duration::from_secs(10);
        let env = "echo $LC_ALL $LVM_SUPPRESS_FD_WARNINGS";
        let out = LVM.run_read("/bin/sh", &["-c", env], long).await.unwrap();
        assert_eq!(out, b"C 1\n");
        let err = LVM
            .run_read("/bin/sh", &["-c", "echo no vg >&2; exit 5"], long)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m == "/bin/sh: no vg"),
            "{err:?}"
        );
    }

    /// 같은 VG 안 복사가 취소돼도(future가 버려짐) 원본 스냅샷 LV를 뒤에서 비활성화한다.
    #[tokio::test]
    async fn cancelled_copy_deactivates_the_source_snapshot() {
        let tools = tempfile::tempdir().unwrap();
        let slow = tools.path().join("slow-qemu-img");
        crate::storage::tests::write_script(&slow, "#!/bin/sh\nsleep 5\n");
        let (lvm, storage) = setup(slow);
        let p = lvm_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        storage.create_volume(&p, src, 1 << 20, RAW).await.unwrap();
        storage.snapshot_create(&p, src, RAW, "t").await.unwrap();
        let snap = format!("opl-{src}-s-t");

        let copy = storage.copy_volume(&p, src, RAW, Some("t"), false, &p, dst, RAW);
        let res = tokio::time::timeout(Duration::from_millis(300), copy).await;
        assert!(res.is_err(), "copy should still be running");
        for _ in 0..200 {
            if !lvm.lv(&snap).unwrap().active {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!lvm.lv(&snap).unwrap().active);
    }

    /// thin pool이 새 블록을 0으로 채우지 않으면(`lvcreate -Zn`) 새 LV의 덜 쓴 블록에 옛 데이터가
    /// 보일 수 있으므로 새 LV에 써 넣을 때(가져오기·복사) `--target-is-zero`를 쓰지 않는다.
    #[tokio::test]
    async fn target_is_zero_needs_thin_pool_zeroing() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup(fake_qemu_img(tools.path(), false));
        let p = lvm_pool();
        assert!(storage.target_reads_zero(&p).await.unwrap());
        *lvm.zero.lock().unwrap() = false;
        assert!(!storage.target_reads_zero(&p).await.unwrap());
        let args = || std::fs::read_to_string(tools.path().join("args")).unwrap();

        let up = Path::new("/up/disk.vmdk");
        storage
            .import_volume(&p, Uuid::now_v7(), up, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert!(args().starts_with("convert\n-q\n-n\n-f\n"), "{}", args());

        // thin pool이 없거나 thin pool이 아니면 오류다.
        let mut root = lvm_pool();
        root.spec.backend = PoolBackend::LvmThin {
            vg: "vg0".into(),
            thinpool: "root".into(),
        };
        let err = storage.target_reads_zero(&root).await.unwrap_err();
        assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
    }

    #[test]
    fn parses_lvs_json() {
        let out = br#"  {
      "report": [
          {
              "lv": [
                  {"lv_name":"tp", "lv_attr":"twi-aotz--", "lv_size":"8589934592", "data_percent":"12.50"},
                  {"lv_name":"opl-x", "lv_attr":"Vwi---tz-k", "lv_size":"1073741824", "data_percent":""},
                  {"lv_name":"root", "lv_attr":"-wi-ao----", "lv_size":4096, "data_percent":null}
              ]
          }
      ]
  }
"#;
        let lvs = parse_lvs(out).unwrap();
        assert_eq!(
            lvs,
            [
                LvInfo {
                    name: "tp".into(),
                    attr: "twi-aotz--".into(),
                    size_bytes: 8 << 30,
                    data_percent: Some(12.5),
                },
                LvInfo {
                    name: "opl-x".into(),
                    attr: "Vwi---tz-k".into(),
                    size_bytes: 1 << 30,
                    data_percent: None,
                },
                LvInfo {
                    name: "root".into(),
                    attr: "-wi-ao----".into(),
                    size_bytes: 4096,
                    data_percent: None,
                },
            ]
        );
        assert!(lvs[0].is_thin_pool() && lvs[0].is_active());
        assert!(lvs[0].zeroes_new_blocks());
        assert!(!lvs[2].zeroes_new_blocks());
        assert_eq!(lvs[0].used_bytes(), 1 << 30);
        assert!(!lvs[1].is_thin_pool() && !lvs[1].is_active());
        assert_eq!(lvs[1].used_bytes(), 0);

        for bad in [
            &b"not json"[..],
            br#"{"report":[{"lv":[{"lv_name":"x","lv_attr":"V","lv_size":"big"}]}]}"#,
            br#"{"report":[{"lv":[{"lv_name":"x","lv_size":"1"}]}]}"#,
            br#"{"nothing":[]}"#,
        ] {
            let err = parse_lvs(bad).unwrap_err();
            assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
        }
    }

    fn shared_pool() -> StoragePool {
        Resource::new(
            "san",
            StoragePoolSpec {
                backend: PoolBackend::LvmShared {
                    vg: "vg0".into(),
                    iscsi: None,
                },
                nodes: vec![],
                shared: true,
            },
        )
    }

    fn setup_shared(qemu_img: impl Into<PathBuf>) -> (Arc<FakeLvm>, LvmSharedStorage) {
        let lvm = Arc::new(FakeLvm::default());
        let storage = LvmSharedStorage {
            dev: lvm.dev.path().to_path_buf(),
            ..LvmSharedStorage::new(qemu_img, lvm.clone())
        };
        (lvm, storage)
    }

    /// 다른 노드가 켜 두어 거절된 일시 오류
    fn held(e: DriverError) {
        assert!(matches!(e, DriverError::Timeout(LV_HELD)), "{e:?}");
        assert!(e.is_transient());
        assert!(e.to_string().contains("another node"), "{e}");
    }

    /// 두꺼운 LV를 켜지 않고 만든 뒤(1 MiB 배수로 올린다) 배타 활성화해 0으로 채우고 끈다.
    #[tokio::test]
    async fn shared_creates_zeroed_thick_lvs() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let path = storage
            .create_volume(&p, id, (1 << 20) + 1, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&lv));
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert!(storage.target_reads_zero(&p).await.unwrap());

        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::VolumeExists(v) if v == id),
            "{err:?}"
        );
        invalid(
            storage
                .create_volume(&p, Uuid::now_v7(), 1 << 20, ImageFormat::Qcow2)
                .await
                .unwrap_err(),
            "lvm-shared pools hold raw volumes",
        );
        assert_eq!(lvm.taken(), Vec::<String>::new());
        // 다른 종류의 풀은 다루지 않는다.
        invalid(
            storage.capacity(&lvm_pool()).await.unwrap_err(),
            "not an lvm-shared pool",
        );
    }

    /// 0으로 채우지 못하면 만든 LV를 지운다.
    #[tokio::test]
    async fn shared_create_removes_the_lv_when_zeroing_fails() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        lvm.fail_on(format!("zero vg0/{lv} 0"));
        let err = storage
            .create_volume(&p, id, 1 << 20, RAW)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 1048576"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("remove vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    /// 이 노드가 0으로 채우는 중인 LV(크기 늘리기)는 끄지 않고 성공이다(채우는 쪽이 켜 둔 LV를 끄면
    /// 채우기가 실패한다. 남은 활성화는 나중의 정리가 끈다).
    #[tokio::test]
    async fn shared_deactivation_skips_a_zeroing_lv() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        lvm.taken();

        let zeroing = format!("vg0/{lv}");
        storage.zeroing.lock().unwrap().insert(zeroing.clone());
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert!(lvm.taken().is_empty());
        assert!(lvm.lv(&lv).unwrap().active);
        storage.zeroing.lock().unwrap().remove(&zeroing);

        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("deactivate vg0/{lv}")]);
        assert!(!lvm.lv(&lv).unwrap().active);
        // 끄는 동안 표시를 쥐었다가 놓는다.
        assert!(storage.zeroing.lock().unwrap().is_empty());
    }

    /// 꺼져 있으면 배타 활성화하고, 다른 노드가 켜 두었으면 일시 오류다. 끄기는 켜져 있을 때만
    /// 끄고, 볼륨이 없거나 이미 꺼져 있으면 성공이다.
    #[tokio::test]
    async fn shared_activation_is_exclusive() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();

        lvm.held.lock().unwrap().push(lv.clone());
        held(storage.activate_volume(&p, id, RAW).await.unwrap_err());
        assert!(!lvm.lv(&lv).unwrap().active);
        lvm.held.lock().unwrap().clear();
        lvm.taken();

        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("activate-ex vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("deactivate vg0/{lv}")]);
        assert!(!lvm.lv(&lv).unwrap().active);
    }

    /// 이 노드에서 켜져 있는 볼륨 LV(`opl-{id}`)만 알린다. 꺼진 볼륨 LV와 opLite 밖의 LV는 빼고 LVM
    /// 명령은 목록뿐이다.
    #[tokio::test]
    async fn shared_lists_volumes_active_on_this_node() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let p = shared_pool();
        let (on, off) = (Uuid::now_v7(), Uuid::now_v7());
        for id in [on, off] {
            storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        }
        storage.activate_volume(&p, on, RAW).await.unwrap();
        lvm.lvs.lock().unwrap().insert(
            format!("opl-{on}-x"),
            FakeLv {
                size: 1 << 20,
                active: true,
                skip: false,
            },
        );
        lvm.taken();
        assert_eq!(storage.active_volumes(&p).await.unwrap(), [on]);
        assert!(lvm.taken().is_empty());
    }

    /// 지우기: 다른 노드가 켜 두었으면 일시 오류, 없으면 `VolumeNotFound`, 없어도 만드는 명령이
    /// 아직 돌고 있으면 일시 오류다.
    #[tokio::test]
    async fn shared_delete_removes_the_lv() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        lvm.held.lock().unwrap().push(lv.clone());
        held(storage.delete_volume(&p, id, RAW).await.unwrap_err());
        assert_eq!(lvm.names(), std::slice::from_ref(&lv));
        lvm.held.lock().unwrap().clear();
        lvm.taken();

        storage.delete_volume(&p, id, RAW).await.unwrap();
        assert_eq!(lvm.taken(), [format!("remove vg0/{lv}")]);
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(matches!(err, DriverError::VolumeNotFound(_)), "{err:?}");
        *lvm.busy.lock().unwrap() = vec![lv];
        let err = storage.delete_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        assert!(lvm.taken().is_empty());
    }

    /// 크기는 늘리기 전 크기를 태그로 적고 늘린 뒤, 새 부분을 배타 활성화해 0으로 채우고 끄고 태그를
    /// 뗀다. 다른 노드가 켜 두었으면 일시 오류다. 정보의 사용량은 크기와 같고, 용량은 VG의 크기와 남은
    /// 공간이다. 스냅샷은 없다.
    #[tokio::test]
    async fn shared_resize_info_capacity_and_no_snapshots() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 1 << 20).await.unwrap();
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
        lvm.held.lock().unwrap().push(lv);
        held(
            storage
                .resize_volume(&p, id, RAW, 3 << 20)
                .await
                .unwrap_err(),
        );

        assert_eq!(
            storage.volume_info(&p, id, RAW).await.unwrap(),
            VolumeInfo {
                virtual_bytes: 2 << 20,
                allocated_bytes: 2 << 20,
            }
        );
        assert_eq!(
            storage.capacity(&p).await.unwrap(),
            Capacity {
                total_bytes: 10 << 30,
                available_bytes: 4 << 30,
            }
        );
        assert!(!storage.supports_snapshots(&p, RAW));
        let no = "lvm-shared pools have no snapshots";
        invalid(
            storage.snapshot_create(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(
            storage.snapshot_revert(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(
            storage.snapshot_delete(&p, id, RAW, "t").await.unwrap_err(),
            no,
        );
        invalid(storage.snapshot_tags(&p, id, RAW).await.unwrap_err(), no);
        invalid(
            storage
                .volume_source(&p, id, RAW, Some("t"))
                .await
                .unwrap_err(),
            no,
        );
    }

    /// 늘린 부분을 0으로 채우지 못하면 오류이고 태그가 남는다. 다시 하면 LV가 이미 그 크기여도 늘리기
    /// 전 크기부터 다시 채운다. 이 노드에서 켜져 있으면(실행 중인 VM) 켠 채로 채운다.
    #[tokio::test]
    async fn shared_resize_zeroes_the_new_part_again_after_a_failure() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        lvm.taken();
        let err = storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.lv(&lv).unwrap().size, 2 << 20);
        assert!(!lvm.lv(&lv).unwrap().active);
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        lvm.fail.lock().unwrap().clear();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );

        storage.activate_volume(&p, id, RAW).await.unwrap();
        lvm.taken();
        storage.resize_volume(&p, id, RAW, 3 << 20).await.unwrap();
        let tag = "opl-zero-from-2097152";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 3145728"),
                format!("zero vg0/{lv} 2097152"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.lv(&lv).unwrap().active);
    }

    /// 크기 늘리기가 늘린 부분을 0으로 다 채우지 못해 남긴 태그는 VM 시작(`activate_volume`)이 장치를
    /// 내주기 전에 마저 채우고 뗀다. 또 채우지 못하면 오류이고 켠 LV를 끈다. 다 채운 뒤에는 할 일이 없다.
    #[tokio::test]
    async fn shared_activation_finishes_a_pending_zero() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        lvm.taken();

        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Lvm(ref m) if m.contains("injected")),
            "{err:?}"
        );
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        lvm.fail.lock().unwrap().clear();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.lv(&lv).unwrap().active);
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
    }

    /// 복제·복사 원본(`volume_source`)도 읽게 하기 전에 남은 0 채우기를 마친다. 채우지 못하면 켠 LV를
    /// 끈다.
    #[tokio::test]
    async fn shared_source_finishes_a_pending_zero() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.fail_on(format!("zero vg0/{lv} 1048576"));
        storage
            .resize_volume(&p, id, RAW, 2 << 20)
            .await
            .unwrap_err();
        lvm.taken();

        storage.volume_source(&p, id, RAW, None).await.unwrap_err();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);

        lvm.fail.lock().unwrap().clear();
        let source = storage.volume_source(&p, id, RAW, None).await.unwrap();
        assert_eq!(source.size_bytes, 2 << 20);
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
        assert!(lvm.tags.lock().unwrap()[&lv].is_empty());
    }

    /// 꺼진 볼륨을 늘리는 중에 같은 노드에서 VM 시작이나 복제 원본이 LV를 켜려 할 수 있다. 이 노드가
    /// 그 LV를 채우는 중이거나(크기 늘리기) 그 LV를 바꾸는 명령이 도는 중이면 켜기는 LV를 건드리지
    /// 않고 일시 오류다(reconciler가 잠시 뒤 다시 한다). 늘리기 전(태그만 붙은
    /// 때)에는 채울 것이 없으므로 태그를 그대로 둔다. 켜기가 남은 부분을 먼저 채웠으면 크기 늘리기는
    /// 다시 채우지 않는다(게스트가 쓴 데이터를 지우지 않는다).
    #[tokio::test]
    async fn shared_activation_during_a_resize() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let tag = "opl-zero-from-1048576";
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        // 늘리기 전에 태그만 붙었다.
        lvm.tags
            .lock()
            .unwrap()
            .insert(lv.clone(), vec![tag.into()]);
        lvm.taken();
        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.deactivate_volume(&p, id, RAW).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert_eq!(lvm.tags.lock().unwrap()[&lv], [tag]);

        // 늘린 뒤
        lvm.lvs.lock().unwrap().get_mut(&lv).unwrap().size = 2 << 20;
        *lvm.busy.lock().unwrap() = vec![lv.clone()];
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(
            matches!(err, DriverError::Timeout(m) if m.contains("still running")),
            "{err:?}"
        );
        lvm.busy.lock().unwrap().clear();
        let zeroing = format!("vg0/{lv}");
        storage.zeroing.lock().unwrap().insert(zeroing.clone());
        let err = storage.activate_volume(&p, id, RAW).await.unwrap_err();
        assert!(err.is_transient(), "{err:?}");
        storage.zeroing.lock().unwrap().remove(&zeroing);
        assert!(lvm.taken().is_empty());
        assert!(!lvm.lv(&lv).unwrap().active);

        storage.activate_volume(&p, id, RAW).await.unwrap();
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
            ]
        );
    }

    /// 크기 늘리기는 [`Zeroing`]을 잡은 뒤 LV를 읽는다. 먼저 읽으면, 읽은 직후 같은 노드의 VM 시작이
    /// LV를 켜 QEMU에 내준 것을 모르고 채운 뒤 끈다. 표시를 먼저 잡으면 그 VM 시작은 일시 오류로
    /// 기다린다.
    #[tokio::test]
    async fn shared_resize_marks_the_lv_before_reading_it() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let storage = Arc::new(storage);
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        // LV를 읽은 직후 VM 시작이 끼어든다: 표시가 없으면 LV를 켜 QEMU에 내준다.
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, flag, name) = (storage.clone(), started.clone(), lv.clone());
        *lvm.on_list.lock().unwrap() = Some(Box::new(move |lvs| {
            if Zeroing::begin(&s.zeroing, "vg0", &name).is_ok() {
                lvs.get_mut(&name).unwrap().active = true;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        storage.resize_volume(&p, id, RAW, 2 << 20).await.unwrap();
        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        let tag = "opl-zero-from-1048576";
        assert_eq!(
            lvm.taken(),
            [
                format!("addtag vg0/{lv} {tag}"),
                format!("extend vg0/{lv} 2097152"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 1048576"),
                format!("deltag vg0/{lv} {tag}"),
                format!("deactivate vg0/{lv}"),
            ]
        );
    }

    /// VM 시작(`activate_volume`)도 [`Zeroing`]을 잡은 뒤 LV를 읽고, 마치지 못하면 자기가 켠 LV만
    /// 끈다. 먼저 읽으면, 읽은 직후 같은 노드의 크기 늘리기가 표시를 잡고 LV를 켠 것을 모르고 일시
    /// 오류에 그 LV를 꺼 채우는 중인 크기 늘리기를 망친다.
    #[tokio::test]
    async fn shared_activation_marks_the_lv_before_reading_it() {
        let (lvm, storage) = setup_shared("/nonexistent/qemu-img");
        let storage = Arc::new(storage);
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        storage.create_volume(&p, id, 1 << 20, RAW).await.unwrap();
        lvm.taken();
        // LV를 읽은 직후 크기 늘리기가 끼어든다: 표시가 비어 있으면 잡고 LV를 켠다.
        let resizing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (s, flag, name) = (storage.clone(), resizing.clone(), lv.clone());
        *lvm.on_list.lock().unwrap() = Some(Box::new(move |lvs| {
            if s.zeroing.lock().unwrap().insert(format!("vg0/{name}")) {
                lvs.get_mut(&name).unwrap().active = true;
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
        storage.activate_volume(&p, id, RAW).await.unwrap();
        assert!(!resizing.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(lvm.taken(), [format!("activate-ex vg0/{lv}")]);
        assert!(lvm.lv(&lv).unwrap().active);
    }

    /// 가져오기: 원본의 가상 크기로 LV를 만들어 0으로 채운 뒤 켠 채로 써 넣고 끈다. 실패하면 지운다.
    #[tokio::test]
    async fn shared_imports_into_a_zeroed_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), false));
        let (p, id) = (shared_pool(), Uuid::now_v7());
        let lv = format!("opl-{id}");
        let src = Path::new("/up/disk.vmdk");
        let path = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap();
        assert_eq!(path, lvm.node(&lv));
        assert_eq!(
            lvm.taken(),
            [
                format!("create-linear vg0 {lv} 1048576"),
                format!("activate-ex vg0/{lv}"),
                format!("zero vg0/{lv} 0"),
                format!("deactivate vg0/{lv}"),
            ]
        );
        assert!(!lvm.lv(&lv).unwrap().active);
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let dev = path.to_str().unwrap();
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
                dev,
            ]
        );

        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), true));
        let err = storage
            .import_volume(&p, id, src, SourceFormat::Vmdk, RAW)
            .await
            .unwrap_err();
        assert!(matches!(err, DriverError::ToolFailed { .. }), "{err:?}");
        assert_eq!(lvm.taken().last().unwrap(), &format!("remove vg0/{lv}"));
        assert_eq!(lvm.names(), Vec::<String>::new());
    }

    /// 같은 VG 안 복사: 원본을 켜서 그 크기로 대상을 만들어(0 채움) 써 넣은 뒤 둘 다 끈다. 원본은
    /// `volume_source`로 켜고 `release_source`로 끈다. 다른 노드가 원본을 켜 두었으면 일시 오류다.
    #[tokio::test]
    async fn shared_copies_into_a_new_lv() {
        let tools = tempfile::tempdir().unwrap();
        let (lvm, storage) = setup_shared(fake_qemu_img(tools.path(), false));
        let p = shared_pool();
        let (src, dst) = (Uuid::now_v7(), Uuid::now_v7());
        let (a, b) = (format!("opl-{src}"), format!("opl-{dst}"));
        storage.create_volume(&p, src, 2 << 20, RAW).await.unwrap();
        lvm.taken();

        let source = storage.volume_source(&p, src, RAW, None).await.unwrap();
        assert_eq!(source.path, lvm.node(&a));
        assert_eq!(source.size_bytes, 2 << 20);
        storage.release_source(&p, src, RAW, None).await.unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{a}"),
                format!("deactivate vg0/{a}")
            ]
        );

        storage
            .copy_volume(&p, src, RAW, None, false, &p, dst, RAW)
            .await
            .unwrap();
        assert_eq!(
            lvm.taken(),
            [
                format!("activate-ex vg0/{a}"),
                format!("create-linear vg0 {b} 2097152"),
                format!("activate-ex vg0/{b}"),
                format!("zero vg0/{b} 0"),
                format!("deactivate vg0/{b}"),
                format!("deactivate vg0/{a}"),
            ]
        );
        let args = std::fs::read_to_string(tools.path().join("args")).unwrap();
        let (from, to) = (lvm.node(&a), lvm.node(&b));
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
                from.to_str().unwrap(),
                to.to_str().unwrap(),
            ]
        );

        lvm.held.lock().unwrap().push(a);
        let other = Uuid::now_v7();
        held(
            storage
                .copy_volume(&p, src, RAW, None, false, &p, other, RAW)
                .await
                .unwrap_err(),
        );
        assert!(!lvm.names().contains(&format!("opl-{other}")));
    }

    /// lvmlockd가 다른 노드의 잠금 때문에 거절하면(LV 또는 VG) 일시 오류다. 다른 LVM 오류는 그대로다.
    #[test]
    fn locks_held_by_other_hosts_are_transient() {
        let lv = held_elsewhere(DriverError::Lvm(
            "lvchange: LV locked by other host: san/opl-x".into(),
        ));
        assert!(matches!(lv, DriverError::Timeout(LV_HELD)), "{lv:?}");
        let vg = held_elsewhere(DriverError::Lvm(
            "lvcreate: VG san lock failed: held by other host.".into(),
        ));
        assert!(matches!(vg, DriverError::Timeout(VG_HELD)), "{vg:?}");
        assert!(vg.is_transient());
        let other = held_elsewhere(DriverError::Lvm("Volume group \"san\" not found".into()));
        assert!(matches!(other, DriverError::Lvm(_)), "{other:?}");
    }

    /// LVM 명령이 잠금 오류 앞에 경고 줄을 내도 잠금 오류로 알아본다.
    #[tokio::test]
    async fn lock_errors_after_warning_lines_are_transient() {
        let long = Duration::from_secs(10);
        for (stderr, want) in [
            (
                "WARNING: retrying ex lock on LV san/opl-x\n  LV locked by other host: san/opl-x",
                LV_HELD,
            ),
            (
                "Retrying ex lock on VG san\n  VG san lock failed: held by other host.",
                VG_HELD,
            ),
        ] {
            let script = format!("printf '{stderr}\\n' >&2; exit 5");
            let err = LVM
                .run_change("/bin/sh", &["-c", &script], &[], long)
                .await
                .unwrap_err();
            let err = held_elsewhere(err);
            assert!(
                matches!(err, DriverError::Timeout(m) if m == want),
                "{err:?}"
            );
        }
    }

    #[test]
    fn parses_lv_tags() {
        assert_eq!(
            parse_lv_tags(b"  opl-zero-from-1048576,other\n"),
            ["opl-zero-from-1048576", "other"]
        );
        assert!(parse_lv_tags(b"  \n").is_empty());
    }

    #[test]
    fn parses_vgs_json() {
        let out = br#"  {
      "report": [
          {
              "vg": [
                  {"vg_name":"san", "vg_lock_type":"sanlock", "vg_size":"10737418240", "vg_free":"4294967296"},
                  {"vg_name":"local", "vg_lock_type":"", "vg_size":4096, "vg_free":0}
              ]
          }
      ]
  }
"#;
        assert_eq!(
            parse_vgs(out).unwrap(),
            [
                VgInfo {
                    name: "san".into(),
                    lock_type: "sanlock".into(),
                    size_bytes: 10 << 30,
                    free_bytes: 4 << 30,
                },
                VgInfo {
                    name: "local".into(),
                    lock_type: String::new(),
                    size_bytes: 4096,
                    free_bytes: 0,
                },
            ]
        );
        for bad in [
            &b"not json"[..],
            br#"{"report":[{"vg":[{"vg_name":"x","vg_size":"big","vg_free":"1"}]}]}"#,
            br#"{"report":[{"vg":[{"vg_name":"x","vg_size":"1"}]}]}"#,
            br#"{"nothing":[]}"#,
        ] {
            let err = parse_vgs(bad).unwrap_err();
            assert!(matches!(err, DriverError::Lvm(_)), "{err:?}");
        }
    }
}
