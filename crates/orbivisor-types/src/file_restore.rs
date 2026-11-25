//! 파일 단위 복원(계획 69): 백업 안 경로·볼륨·항목의 API 보기, 게스트로 되돌려 쓰기의 이름 규칙, 세션 노드 고르기.
//! 세션 상태(볼륨 목록·나열 결과)는 세션 노드의 메모리에만 있고 Raft에는 Task 레코드와 `NodeStatus.file_restore`만
//! 간다. 볼륨·항목의 이름·크기는 도우미 VM(믿을 수 없는 백업 안 파일시스템)에서 오므로 opld가 이 모듈의 한도로 거른다.

use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

use crate::vm::Arch;

/// 경로 조각(파일 이름) 하나의 최대 길이(바이트). 리눅스 파일시스템의 255바이트보다 넉넉하다(NTFS UTF-16 → UTF-8).
pub const MAX_NAME_BYTES: usize = 1024;
/// 경로 깊이(조각 수) 상한
pub const MAX_PATH_DEPTH: usize = 256;
/// 심볼릭 링크 대상 길이 상한(바이트)
pub const MAX_LINK_TARGET: usize = 4096;
/// 세션 하나의 볼륨 수 상한
pub const MAX_VOLUMES: usize = 256;
/// 나열 한 쪽의 항목 수 상한
pub const MAX_PAGE_ENTRIES: usize = 1000;
/// LVM 이름(VG·LV) 길이 상한(바이트, LVM 자체 한도)
pub const MAX_LVM_NAME: usize = 127;
/// 같은 이름이 있을 때 `keep`이 붙여 보는 이름 수 상한(`.restored-…`, `-2` … `-99`)
pub const MAX_KEEP_NAMES: u32 = 99;

// ---------------------------------------------------------------------------------------------------------------
// 경로
// ---------------------------------------------------------------------------------------------------------------

/// 백업 안 경로가 잘못됐다(API 400 `invalid_path`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathError {
    #[error("path has an empty component (use \"/a/b\", not \"/a//b\" or \"/a/\")")]
    Empty,
    #[error("path components \".\" and \"..\" are not allowed")]
    Dot,
    #[error("path component contains '/' or NUL")]
    Separator,
    #[error("path component is longer than {MAX_NAME_BYTES} bytes")]
    LongName,
    #[error("path is deeper than {MAX_PATH_DEPTH} components")]
    Deep,
    #[error("path component {{\"b64\": …}} is not valid base64")]
    Base64,
}

/// 경로 조각(파일 이름) 하나. 파일시스템 이름은 바이트열이라(ext4·XFS) UTF-8이 아닐 수 있다. JSON은 UTF-8이면 문자열,
/// 아니면 `{ "b64": "…" }`(표준 base64)이다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathPart(Vec<u8>);

impl PathPart {
    /// 검사한 조각: 비지 않고 `.`·`..`가 아니며 `/`·NUL이 없고 [`MAX_NAME_BYTES`] 안이다.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, PathError> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return Err(PathError::Empty);
        }
        if bytes == b"." || bytes == b".." {
            return Err(PathError::Dot);
        }
        if bytes.iter().any(|b| *b == b'/' || *b == 0) {
            return Err(PathError::Separator);
        }
        if bytes.len() > MAX_NAME_BYTES {
            return Err(PathError::LongName);
        }
        Ok(PathPart(bytes))
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// UTF-8이면 그 문자열
    pub fn to_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }

    /// 보이기용 이름(UTF-8이 아닌 바이트는 U+FFFD)
    pub fn lossy(&self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct B64Part {
    b64: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RawPart {
    Text(String),
    Raw(B64Part),
}

impl RawPart {
    fn into_part(self) -> Result<PathPart, PathError> {
        match self {
            RawPart::Text(s) => PathPart::new(s),
            RawPart::Raw(B64Part { b64 }) => {
                PathPart::new(STANDARD.decode(b64).map_err(|_| PathError::Base64)?)
            }
        }
    }
}

impl From<&PathPart> for RawPart {
    fn from(p: &PathPart) -> Self {
        match p.to_str() {
            Some(s) => RawPart::Text(s.to_string()),
            None => RawPart::Raw(B64Part {
                b64: STANDARD.encode(&p.0),
            }),
        }
    }
}

impl Serialize for PathPart {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        RawPart::from(self).serialize(s)
    }
}

impl<'de> Deserialize<'de> for PathPart {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        RawPart::deserialize(d)?
            .into_part()
            .map_err(serde::de::Error::custom)
    }
}

/// 볼륨 루트에서 시작하는 백업 안 경로(조각 목록, 빈 목록은 루트). JSON은 조각 목록이고, 읽을 때는 문자열
/// `"/etc/nginx"`도 받아 `/`로 나눈다(UTF-8 조각만 — 앞의 `/` 하나는 있어도 없어도 된다).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct EntryPath(Vec<PathPart>);

impl EntryPath {
    /// 볼륨 루트
    pub fn root() -> Self {
        EntryPath(Vec::new())
    }

    /// 조각 목록(깊이 검사)
    pub fn from_parts(parts: Vec<PathPart>) -> Result<Self, PathError> {
        if parts.len() > MAX_PATH_DEPTH {
            return Err(PathError::Deep);
        }
        Ok(EntryPath(parts))
    }

    /// `"/etc/nginx"`·`"etc/nginx"`·`"/"`(루트)·`""`(루트). 빈 조각(`//`·끝 `/`)·`.`·`..`는 거절한다.
    pub fn parse(s: &str) -> Result<Self, PathError> {
        let rest = s.strip_prefix('/').unwrap_or(s);
        if rest.is_empty() {
            return Ok(EntryPath::root());
        }
        let parts = rest
            .split('/')
            .map(PathPart::new)
            .collect::<Result<Vec<_>, _>>()?;
        EntryPath::from_parts(parts)
    }

    /// 보이기용 `/a/b`(루트는 `/`, UTF-8이 아닌 바이트는 U+FFFD). 감사·로그는 이것을 더 자른다.
    pub fn display(&self) -> String {
        if self.0.is_empty() {
            return "/".to_string();
        }
        self.0.iter().fold(String::new(), |mut s, p| {
            s.push('/');
            s.push_str(&String::from_utf8_lossy(p.as_bytes()));
            s
        })
    }

    pub fn parts(&self) -> &[PathPart] {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// 마지막 조각(루트는 없음)
    pub fn last(&self) -> Option<&PathPart> {
        self.0.last()
    }

    /// 부모 경로(루트의 부모는 루트)
    pub fn parent(&self) -> EntryPath {
        let mut parts = self.0.clone();
        parts.pop();
        EntryPath(parts)
    }

    /// 조각 하나를 붙인 경로
    pub fn join(&self, part: PathPart) -> Result<EntryPath, PathError> {
        let mut parts = self.0.clone();
        parts.push(part);
        EntryPath::from_parts(parts)
    }
}

impl fmt::Display for EntryPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawPath {
    Text(String),
    Parts(Vec<RawPart>),
}

impl Serialize for EntryPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.0.iter().map(RawPart::from))
    }
}

impl<'de> Deserialize<'de> for EntryPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let path = match RawPath::deserialize(d)? {
            RawPath::Text(s) => EntryPath::parse(&s),
            RawPath::Parts(parts) => parts
                .into_iter()
                .map(RawPart::into_part)
                .collect::<Result<Vec<_>, _>>()
                .and_then(EntryPath::from_parts),
        };
        path.map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "openapi")]
fn path_part_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Type};
    OneOfBuilder::new()
        .item(ObjectBuilder::new().schema_type(Type::String))
        .item(
            ObjectBuilder::new()
                .property(
                    "b64",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .description(Some("standard base64 of a name that is not UTF-8")),
                )
                .required("b64"),
        )
        .description(Some(
            "one file name: a string, or {\"b64\": …} when the name is not UTF-8",
        ))
        .into()
}

// JSON 모양이 구조체와 달라 API 문서 스키마를 손으로 적는다.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for PathPart {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        path_part_schema()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for PathPart {}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for EntryPath {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ArrayBuilder, ObjectBuilder, OneOfBuilder, Type};
        OneOfBuilder::new()
            .item(
                ArrayBuilder::new()
                    .items(path_part_schema())
                    .max_items(Some(MAX_PATH_DEPTH)),
            )
            .item(ObjectBuilder::new().schema_type(Type::String))
            .description(Some(
                "a path inside a volume: a list of names (empty = the volume root), or a string \"/etc/nginx\" \
                 (UTF-8 names only; written back as a list)",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for EntryPath {}

// ---------------------------------------------------------------------------------------------------------------
// 볼륨
// ---------------------------------------------------------------------------------------------------------------

/// 볼륨 id `disk{i}`: 파티션 표 없이 디스크 전체가 파일시스템·PV
pub fn disk_volume_id(disk: u32) -> String {
    format!("disk{disk}")
}

/// 볼륨 id `disk{i}p{n}`(파티션 번호 — MBR 논리 파티션은 5부터)
pub fn partition_volume_id(disk: u32, number: u32) -> String {
    format!("disk{disk}p{number}")
}

/// LVM 이름 규칙(`[A-Za-z0-9+_.-]`, 1~127바이트, `-`로 시작하지 않음, `.`·`..`이 아님)
fn lvm_name_ok(s: &str) -> bool {
    (1..=MAX_LVM_NAME).contains(&s.len())
        && !s.starts_with('-')
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'_' | b'.' | b'-'))
}

/// 볼륨 id `lvm/{vg}/{lv}`. 이름이 LVM 규칙에 어긋나면(게스트가 보낸 값) UUID로 `lvm/{vg uuid}/{lv uuid}`. UUID도
/// 어긋나면 없다(그 LV는 보이지 않는다).
pub fn lv_volume_id(vg: &str, lv: &str, vg_uuid: &str, lv_uuid: &str) -> Option<String> {
    if lvm_name_ok(vg) && lvm_name_ok(lv) {
        Some(format!("lvm/{vg}/{lv}"))
    } else if lvm_name_ok(vg_uuid) && lvm_name_ok(lv_uuid) {
        Some(format!("lvm/{vg_uuid}/{lv_uuid}"))
    } else {
        None
    }
}

/// 10진 수(앞 0 없음, u32 안)
fn canonical_u32(s: &str) -> bool {
    s.parse::<u32>().is_ok_and(|n| n.to_string() == s)
}

/// API가 받은 볼륨 id가 [`disk_volume_id`]·[`partition_volume_id`]·[`lv_volume_id`]의 꼴인지
pub fn valid_volume_id(id: &str) -> bool {
    if let Some(rest) = id.strip_prefix("lvm/") {
        return rest
            .split_once('/')
            .is_some_and(|(vg, lv)| lvm_name_ok(vg) && lvm_name_ok(lv));
    }
    let Some(rest) = id.strip_prefix("disk") else {
        return false;
    };
    match rest.split_once('p') {
        Some((disk, part)) => canonical_u32(disk) && canonical_u32(part),
        None => canonical_u32(rest),
    }
}

/// 볼륨 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum VolumeKind {
    Disk,
    Partition,
    LogicalVolume,
}

/// 파티션 표 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PartitionTable {
    Mbr,
    Gpt,
}

/// 파티션 정보
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PartitionInfo {
    pub table: PartitionTable,
    /// 파티션 번호(1부터, MBR 논리 파티션은 5부터)
    pub number: u32,
    /// 파티션 종류 이름(예: `Linux filesystem`, `EFI System`, `0x83`)
    pub type_name: String,
    /// GPT 파티션 UUID(MBR은 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub uuid: Option<String>,
}

/// 볼륨 마운트 상태
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum VolumeState {
    Unmounted,
    Mounted,
    Failed,
}

/// 세션의 볼륨 하나(디스크·파티션·LVM 논리 볼륨). 도우미가 탐지하고 opld가 한도를 검사한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VolumeView {
    /// `disk0`, `disk0p2`, `lvm/rl/root`(세션 안에서만 뜻이 있다)
    pub id: String,
    /// 백업 디스크 번호(LV는 첫 PV의 디스크)
    pub disk: u32,
    pub kind: VolumeKind,
    /// 크기(바이트)
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub partition: Option<PartitionInfo>,
    /// 파일시스템·내용 종류(blkid `TYPE` — `ext4`, `xfs`, `LVM2_member`, `crypto_LUKS` …). 모르면 없음
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub fs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub uuid: Option<String>,
    /// 찾아볼 수 있는 파일시스템인지(LVM PV·암호화·swap·모르는 것은 거짓)
    pub mountable: bool,
    pub state: VolumeState,
    /// 덧붙임(예: `journal not replayed — recent changes may be missing`, `encrypted (LUKS) — not supported`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub note: Option<String>,
}

// ---------------------------------------------------------------------------------------------------------------
// 항목
// ---------------------------------------------------------------------------------------------------------------

/// 디렉터리 항목 종류(장치·FIFO·소켓은 `other`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 디렉터리 항목 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct EntryView {
    /// 이름(UTF-8이 아닌 바이트는 U+FFFD)
    pub name: String,
    /// 이름이 UTF-8이 아니면 원래 바이트의 표준 base64(경로 조각 `{ "b64": … }`로 되보낸다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub raw: Option<String>,
    pub kind: EntryKind,
    /// 크기(바이트, 일반 파일)
    pub size: u64,
    /// 수정 시각(유닉스 초, 1970 전이면 음수)
    pub mtime: i64,
    /// 권한 비트(`st_mode & 0o7777`)
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// 심볼릭 링크 대상(따라가지 않는다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub link_target: Option<String>,
}

impl EntryView {
    /// 이 항목의 경로 조각(되보낼 때). `raw`가 있으면 그 바이트, 아니면 `name`
    pub fn part(&self) -> Result<PathPart, PathError> {
        match &self.raw {
            Some(b64) => PathPart::new(STANDARD.decode(b64).map_err(|_| PathError::Base64)?),
            None => PathPart::new(self.name.clone()),
        }
    }
}

/// 디렉터리 나열 한 쪽
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListPage {
    pub entries: Vec<EntryView>,
    /// 다음 쪽 커서. 없으면 마지막 쪽
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub next: Option<String>,
}

// ---------------------------------------------------------------------------------------------------------------
// 세션
// ---------------------------------------------------------------------------------------------------------------

/// 세션 단계
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// 도우미 VM이 부팅·탐지 중
    Booting,
    Ready,
    Failed,
    Closed,
}

/// 파일 복원 세션(API 보기). 세션은 Task `file-restore-session`이고 볼륨 목록은 세션 노드 메모리에 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileRestoreSessionView {
    /// 세션 id(= Task 대상)
    pub id: Uuid,
    pub backup: Uuid,
    pub backup_name: String,
    /// 세션 노드
    pub node: Uuid,
    pub phase: SessionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 연 사용자(토큰이면 토큰 주인)
    pub opened_by: Uuid,
    /// 연 시각·마지막으로 쓴 시각·닫힐 시각(유닉스 초 — 유휴와 최대 시간 가운데 이른 것)
    pub opened_at: u64,
    pub last_used_at: u64,
    pub expires_at: u64,
    /// 볼륨(부팅 중이면 빈 목록)
    #[serde(default)]
    pub volumes: Vec<VolumeView>,
}

/// 게스트 OS 갈래(되돌려 쓸 경로 규칙)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuestOsKind {
    Linux,
    Windows,
    Other,
}

/// 대상 VM의 게스트 에이전트가 허락하는 것(`guest-info.supported_commands`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestRestoreCaps {
    pub os: GuestOsKind,
    /// `guest-file-open`·`guest-file-write`·`guest-file-close`
    pub file_write: bool,
    /// `guest-exec`·`guest-exec-status`(디렉터리 만들기·덮어쓰기)
    pub exec: bool,
    /// 못 하는 까닭(예: RHEL 에이전트의 기본 막힘)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub reason: Option<String>,
}

/// 게스트에 같은 이름이 있을 때
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum GuestConflict {
    /// 있는 파일을 두고 `<이름>.restored-YYYYMMDD-HHMMSS`로 쓴다.
    #[default]
    Keep,
    /// 건너뛴다.
    Skip,
    /// 임시 이름에 다 쓴 뒤 바꿔 넣는다(`guest-exec`가 있어야 한다).
    Overwrite,
}

// ---------------------------------------------------------------------------------------------------------------
// 노드
// ---------------------------------------------------------------------------------------------------------------

/// 노드의 파일 복원 능력(`NodeStatus.file_restore`): 도우미 이미지가 있고 검사를 지난 노드만 알린다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileRestoreNode {
    /// 이미지 판(`manifest.json`)
    pub image_version: String,
    /// 이미지·도우미 VM 아키텍처
    pub arch: Arch,
    /// 도우미 VM 가속기(`kvm`·`hvf`·`tcg` — 다른 저장 칸처럼 문자열이라 모르는 값이 와도 상태를 푼다)
    pub accel: String,
    /// 지금 열린 세션 수
    pub sessions: u32,
    /// 노드당 세션 상한(`[file_restore] max_sessions`)
    pub max_sessions: u32,
}

impl FileRestoreNode {
    /// 하드웨어 가속(KVM·HVF)이면 참, TCG·모르는 값이면 거짓
    pub fn accelerated(&self) -> bool {
        matches!(
            crate::cpu::Accel::parse(&self.accel),
            Some(crate::cpu::Accel::Kvm | crate::cpu::Accel::Hvf)
        )
    }
}

/// 세션 노드 후보 하나([`choose_session_node`]의 입력)
#[derive(Debug, Clone, Copy)]
pub struct SessionNodeCandidate<'a> {
    pub id: Uuid,
    pub name: &'a str,
    /// 온라인이고 Task를 돌리는 노드(위트니스가 아님)
    pub online: bool,
    /// 이 노드가 백업을 읽을 수 있는지(저장소 `usable_on_node`, 풀 백업은 그 풀)
    pub reads_backup: bool,
    pub file_restore: Option<&'a FileRestoreNode>,
}

/// 세션을 열 노드가 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum NoSessionNode {
    #[error("no online node has the file restore image (install orbivisor-restore-image)")]
    NoImage,
    #[error(
        "no node that can read this backup has the file restore image (install orbivisor-restore-image there)"
    )]
    NoReader,
    #[error(
        "every node that can open this backup already runs its maximum number of file restore sessions"
    )]
    Busy,
}

impl NoSessionNode {
    /// API 오류 코드(409)
    pub fn code(self) -> &'static str {
        match self {
            NoSessionNode::Busy => "file_restore_busy",
            NoSessionNode::NoImage | NoSessionNode::NoReader => "file_restore_unavailable",
        }
    }
}

/// 세션 노드 고르기(설계 §10.2): 이미지가 있고 온라인이고 백업을 읽고 자리가 남은 노드 가운데 API를 받은 노드 →
/// 세션이 적은 노드 → 가속(KVM·HVF)이 있는 노드 → 이름 차례.
pub fn choose_session_node(
    candidates: &[SessionNodeCandidate<'_>],
    api_node: Uuid,
) -> Result<Uuid, NoSessionNode> {
    let with_image = candidates
        .iter()
        .filter(|c| c.online)
        .filter_map(|c| c.file_restore.map(|fr| (c, fr)));
    let readers: Vec<_> = with_image.clone().filter(|(c, _)| c.reads_backup).collect();
    if readers.is_empty() {
        return Err(if with_image.count() == 0 {
            NoSessionNode::NoImage
        } else {
            NoSessionNode::NoReader
        });
    }
    readers
        .into_iter()
        .filter(|(_, fr)| fr.sessions < fr.max_sessions)
        .min_by_key(|(c, fr)| (c.id != api_node, fr.sessions, !fr.accelerated(), c.name))
        .map(|(c, _)| c.id)
        .ok_or(NoSessionNode::Busy)
}

// ---------------------------------------------------------------------------------------------------------------
// 게스트로 되돌려 쓰기
// ---------------------------------------------------------------------------------------------------------------

/// 게스트에 쓸 이름이 그 OS에서 쓸 수 없다(되돌려 쓰기는 그 항목을 건너뛰고 경고).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GuestPathError {
    #[error("name is empty, \".\" or \"..\"")]
    Empty,
    #[error("name contains NUL or '/'")]
    Separator,
    #[error(
        "name contains a character Windows does not allow (<>:\"/\\|?* or a control character)"
    )]
    WindowsChar,
    #[error("Windows names cannot end with a space or a dot")]
    WindowsTrailing,
    #[error("{0:?} is a reserved device name on Windows")]
    WindowsReserved(String),
}

const WINDOWS_RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

/// 경로 조각 하나를 그 게스트 OS에서 쓸 수 있는지(설계 §12.2). Linux·기타는 NUL·`/`만 막고, Windows는 금지 문자
/// `<>:"/\|?*`·제어 문자·끝 공백/점·예약 이름(`CON`·`PRN`·`AUX`·`NUL`·`COM1`~`9`·`LPT1`~`9`, 확장자가 붙어도)을 막는다.
/// 거른 이름만 `guest-exec`의 argv·PowerShell `$args[0]`로 넘어간다(따옴표·제어 문자가 없다).
pub fn guest_path_ok(os: GuestOsKind, part: &str) -> Result<(), GuestPathError> {
    if part.is_empty() || part == "." || part == ".." {
        return Err(GuestPathError::Empty);
    }
    if part.contains(['\0', '/']) {
        return Err(GuestPathError::Separator);
    }
    if os != GuestOsKind::Windows {
        return Ok(());
    }
    if part
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*'))
    {
        return Err(GuestPathError::WindowsChar);
    }
    if part.ends_with([' ', '.']) {
        return Err(GuestPathError::WindowsTrailing);
    }
    let stem = part.split('.').next().unwrap_or(part).trim_end();
    let upper = stem.to_ascii_uppercase();
    let numbered = (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.len() == 4
        && matches!(upper.as_bytes()[3], b'1'..=b'9');
    if WINDOWS_RESERVED.contains(&upper.as_str()) || numbered {
        return Err(GuestPathError::WindowsReserved(part.to_string()));
    }
    Ok(())
}

/// 게스트에 파일 하나를 어떻게 쓸지([`guest_target_name`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestTarget {
    /// 이 이름으로 새로 쓴다(없던 이름, 또는 `keep`이 고른 이름).
    Write(String),
    /// 건너뛴다(`skip`).
    Skip,
    /// 임시 이름([`guest_temp_name`])에 쓴 뒤 원래 이름으로 바꿔 넣는다(`overwrite`).
    Overwrite,
}

/// `keep`이 붙여 볼 이름이 모두 있다.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0:?} and all of its {MAX_KEEP_NAMES} .restored-* names already exist")]
pub struct NoFreeName(pub String);

/// `at`(유닉스 초)의 UTC `YYYYMMDD-HHMMSS`
fn stamp(at: u64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(at.min(i64::MAX as u64) as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// 같은 이름이 있을 때 쓸 이름(설계 §12.2 3): 없으면 그 이름, `keep`은 `<이름>.restored-YYYYMMDD-HHMMSS`(확장자 뒤에
/// 붙임 — `a.conf` → `a.conf.restored-20261124-101500`, 그것도 있으면 `-2` … `-99`), `skip`은 건너뜀, `overwrite`는 바꿔
/// 넣기. `exists`는 게스트에 그 이름이 있는지(같은 디렉터리)다.
pub fn guest_target_name(
    name: &str,
    conflict: GuestConflict,
    mut exists: impl FnMut(&str) -> bool,
    now: u64,
) -> Result<GuestTarget, NoFreeName> {
    if !exists(name) {
        return Ok(GuestTarget::Write(name.to_string()));
    }
    match conflict {
        GuestConflict::Skip => Ok(GuestTarget::Skip),
        GuestConflict::Overwrite => Ok(GuestTarget::Overwrite),
        GuestConflict::Keep => guest_keep_names(name, now)
            .find(|candidate| !exists(candidate))
            .map(GuestTarget::Write)
            .ok_or_else(|| NoFreeName(name.to_string())),
    }
}

/// `keep`이 차례로 붙여 볼 이름: `<이름>.restored-YYYYMMDD-HHMMSS`, 그다음 `-2` … `-99`([`MAX_KEEP_NAMES`]개)
pub fn guest_keep_names(name: &str, now: u64) -> impl Iterator<Item = String> + use<> {
    let base = format!("{name}.restored-{}", stamp(now));
    (1..=MAX_KEEP_NAMES).map(move |n| match n {
        1 => base.clone(),
        n => format!("{base}-{n}"),
    })
}

/// `overwrite`가 먼저 쓰는 임시 이름 `<이름>.orbivisor-<task>`(다 쓴 뒤 원래 이름으로 바꾼다 — 끊겨도 원래 파일은 그대로)
pub fn guest_temp_name(name: &str, task: Uuid) -> String {
    format!("{name}.orbivisor-{task}")
}

/// 게스트에 한 번에 쓰는 날 바이트(`guest-file-write` 하나 — base64로 1.4 MiB 줄, 에이전트 잠금을 이만큼마다 놓는다)
pub const GUEST_WRITE_CHUNK: usize = 1 << 20;
/// Windows 경로 길이 상한(`\\?\` 없이 — `MAX_PATH` 260에서 끝 NUL을 뺀 글자 수)
pub const MAX_WINDOWS_PATH: usize = 259;

/// 게스트 에이전트에 하는 파일 일 하나(계획 69 §12). 하이퍼바이저(`guest_file`)와 노드 사이 RPC `GuestAgent`가 그대로
/// 나른다. 일마다 VM의 에이전트 잠금을 쥐고 놓는다 — 큰 파일을 쓰는 동안에도 게스트 정보 수집·fs-freeze가 기다리지 않게.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GuestFileOp {
    /// 에이전트가 허락하는 것(`guest-info.supported_commands`)과 OS(`guest-get-osinfo`)
    Caps,
    /// 그 경로를 읽기로 열 수 있는지(`guest-file-open mode=r` → `guest-file-close`)
    Exists { path: String },
    /// 쓰기로 연다(`guest-file-open mode=wb` — 있으면 비운다) → 핸들
    Open { path: String },
    /// 핸들에 쓴다(`guest-file-write`, 날 바이트의 표준 base64 — [`GUEST_WRITE_CHUNK`]까지)
    Write { handle: i64, data: String },
    /// 비우고 닫는다(`guest-file-flush` → `guest-file-close`)
    Close { handle: i64 },
    /// 셸 없이 프로그램을 돌려 끝날 때까지 기다린다(`guest-exec` → `guest-exec-status`)
    Exec { path: String, args: Vec<String> },
}

/// 쓰는 데이터는 길이만 적는다.
impl fmt::Debug for GuestFileOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caps => f.write_str("Caps"),
            Self::Exists { path } => f.debug_struct("Exists").field("path", path).finish(),
            Self::Open { path } => f.debug_struct("Open").field("path", path).finish(),
            Self::Write { handle, data } => f
                .debug_struct("Write")
                .field("handle", handle)
                .field("base64_len", &data.len())
                .finish(),
            Self::Close { handle } => f.debug_struct("Close").field("handle", handle).finish(),
            Self::Exec { path, args } => f
                .debug_struct("Exec")
                .field("path", path)
                .field("args", args)
                .finish(),
        }
    }
}

/// [`GuestFileOp`]의 답. 에이전트가 오류로 답한 일(없는 디렉터리, 권한, 막힌 명령)은 `Refused`다 — 채널은 살아 있다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum GuestFileAnswer {
    Caps {
        caps: GuestRestoreCaps,
    },
    Exists {
        exists: bool,
    },
    Opened {
        handle: i64,
    },
    Written {
        count: u64,
    },
    Closed,
    /// 프로그램이 끝났다: 종료 코드(시그널이면 128+번호)와 표준 오류(1 KiB까지)
    Exited {
        code: i64,
        stderr: String,
    },
    Refused {
        message: String,
    },
}

/// 에이전트가 허락하는 명령(`guest-info.supported_commands`의 이름과 `enabled`)으로 되돌려 쓰기 능력을 정한다. 파일 쓰기는
/// `guest-file-open`·`-write`·`-flush`·`-close`, 디렉터리 만들기·덮어쓰기는 `guest-exec`·`guest-exec-status`가 든다.
pub fn guest_restore_caps(os: GuestOsKind, commands: &[(String, bool)]) -> GuestRestoreCaps {
    let enabled = |name: &str| commands.iter().any(|(n, on)| n == name && *on);
    let file_write = [
        "guest-file-open",
        "guest-file-write",
        "guest-file-flush",
        "guest-file-close",
    ]
    .iter()
    .all(|c| enabled(c));
    let exec = enabled("guest-exec") && enabled("guest-exec-status");
    let reason = if !file_write {
        Some(
            "the guest agent blocks guest-file-* (the default on RHEL/Rocky: allow them in /etc/sysconfig/qemu-ga)"
                .to_string(),
        )
    } else if !exec {
        Some(
            "the guest agent blocks guest-exec: only files directly in an existing directory can be restored, \
             and overwrite is not available"
                .to_string(),
        )
    } else {
        None
    };
    GuestRestoreCaps {
        os,
        file_write,
        exec,
        reason,
    }
}

/// 게스트 OS 갈래: `guest-get-osinfo`의 `id`가 `mswindows`면 Windows, 답했으면 Linux(POSIX — 같은 규칙), 답하지 않으면 기타
pub fn guest_os_kind(osinfo_id: Option<&str>, answered: bool) -> GuestOsKind {
    match osinfo_id {
        Some("mswindows") => GuestOsKind::Windows,
        _ if answered => GuestOsKind::Linux,
        _ => GuestOsKind::Other,
    }
}

/// 게스트 안 대상 디렉터리(거른 조각). [`GuestDir::parse`]로 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestDir {
    os: GuestOsKind,
    /// Windows 드라이브(`C:`), 아니면 빈 값
    drive: String,
    parts: Vec<String>,
}

impl GuestDir {
    /// 사용자가 준 대상 디렉터리: Linux·기타는 `/`로 시작하는 절대 경로, Windows는 `C:\…`(`/`도 받음). 빈 조각(`//`·끝
    /// `/`)은 버리고, 조각마다 [`guest_path_ok`]를 지나야 한다(`.`·`..` 없음). Windows는 260자 안.
    pub fn parse(os: GuestOsKind, s: &str) -> Result<Self, String> {
        let (drive, rest) = if os == GuestOsKind::Windows {
            let b = s.as_bytes();
            if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
                return Err(format!(
                    "{s:?} is not an absolute Windows path (like C:\\restore)"
                ));
            }
            let rest = &s[2..];
            if !rest.is_empty() && !rest.starts_with(['\\', '/']) {
                return Err(format!(
                    "{s:?} is not an absolute Windows path (like C:\\restore)"
                ));
            }
            (s[..2].to_ascii_uppercase(), rest)
        } else {
            let Some(rest) = s.strip_prefix('/') else {
                return Err(format!("{s:?} is not an absolute path"));
            };
            (String::new(), rest)
        };
        let seps: &[char] = if os == GuestOsKind::Windows {
            &['\\', '/']
        } else {
            &['/']
        };
        let mut parts = Vec::new();
        for part in rest.split(seps).filter(|p| !p.is_empty()) {
            guest_path_ok(os, part).map_err(|e| format!("{part:?} in {s:?}: {e}"))?;
            parts.push(part.to_string());
        }
        let dir = GuestDir { os, drive, parts };
        dir.join(&[]).map_err(|e| format!("{s:?}: {e}"))?;
        Ok(dir)
    }

    pub fn os(&self) -> GuestOsKind {
        self.os
    }

    /// 이 디렉터리
    pub fn path(&self) -> String {
        self.join(&[]).unwrap_or_default()
    }

    /// 아래 경로(조각은 이미 [`guest_path_ok`]를 지난 것). Windows에서 260자를 넘으면 오류다.
    pub fn join(&self, more: &[String]) -> Result<String, String> {
        let all: Vec<&str> = self.parts.iter().chain(more).map(String::as_str).collect();
        if self.os == GuestOsKind::Windows {
            let path = format!("{}\\{}", self.drive, all.join("\\"));
            if path.chars().count() > MAX_WINDOWS_PATH {
                return Err(format!(
                    "the Windows path is longer than {MAX_WINDOWS_PATH} characters"
                ));
            }
            Ok(path)
        } else {
            Ok(format!("/{}", all.join("/")))
        }
    }
}

/// PowerShell 작은따옴표 문자열(`'`와 같은 뜻의 ‘ ’ ‚ ‛는 두 번 — 그 밖의 글자는 그대로 글자다)
fn ps_quote(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// PowerShell 스크립트를 명령줄 해석 없이 넘긴다(`-EncodedCommand` — UTF-16LE의 base64).
fn powershell(script: &str) -> (String, Vec<String>) {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    (
        "powershell.exe".into(),
        vec![
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            STANDARD.encode(utf16),
        ],
    )
}

/// 디렉터리 만들기(`guest-exec` argv — 셸 없음): Linux·기타 `/bin/mkdir -p -- <경로>`, Windows `New-Item -ItemType
/// Directory -Force`
pub fn guest_mkdir_command(os: GuestOsKind, path: &str) -> (String, Vec<String>) {
    if os == GuestOsKind::Windows {
        return powershell(&format!(
            "$ErrorActionPreference='Stop'; New-Item -ItemType Directory -Force -Path {} | Out-Null",
            ps_quote(path)
        ));
    }
    (
        "/bin/mkdir".into(),
        vec!["-p".into(), "--".into(), path.into()],
    )
}

/// 다 쓴 임시 파일을 원래 이름으로 바꿔 넣기(`overwrite`): Linux·기타 `/bin/mv -f -T -- <임시> <이름>`(이름이 디렉터리면
/// 실패 — 그 안으로 옮기지 않음), Windows `Move-Item -Force -LiteralPath`
pub fn guest_move_command(os: GuestOsKind, from: &str, to: &str) -> (String, Vec<String>) {
    if os == GuestOsKind::Windows {
        return powershell(&format!(
            "$ErrorActionPreference='Stop'; Move-Item -Force -LiteralPath {} -Destination {}",
            ps_quote(from),
            ps_quote(to)
        ));
    }
    (
        "/bin/mv".into(),
        vec![
            "-f".into(),
            "-T".into(),
            "--".into(),
            from.into(),
            to.into(),
        ],
    )
}

/// 파일 지우기(끊긴 쓰기의 임시 파일): Linux·기타 `/bin/rm -f -- <경로>`, Windows `Remove-Item -Force -LiteralPath`
pub fn guest_remove_command(os: GuestOsKind, path: &str) -> (String, Vec<String>) {
    if os == GuestOsKind::Windows {
        return powershell(&format!(
            "Remove-Item -Force -ErrorAction SilentlyContinue -LiteralPath {}",
            ps_quote(path)
        ));
    }
    (
        "/bin/rm".into(),
        vec!["-f".into(), "--".into(), path.into()],
    )
}

/// 게스트로 되돌려 쓰기 요청(`POST /file-restore/sessions/{id}/restore-to-vm`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct RestoreToVmRequest {
    /// 세션의 볼륨 id
    pub volume: String,
    /// 되돌려 쓸 항목(1~1000개, 같은 볼륨)
    pub paths: Vec<EntryPath>,
    /// 대상 VM(id나 이름) — 실행 중이고 게스트 에이전트 채널이 있어야 한다
    pub vm: String,
    /// 게스트 안 대상 디렉터리(Linux `/srv/restored`, Windows `C:\\restored`). 고른 항목의 이름이 그 아래에 놓인다.
    pub target_dir: String,
    /// 같은 이름이 있을 때(기본 `keep`)
    #[serde(default)]
    pub conflict: GuestConflict,
}

// ---------------------------------------------------------------------------------------------------------------
// 내려받기
// ---------------------------------------------------------------------------------------------------------------

/// 내려받기 형식
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum DownloadFormat {
    /// 일반 파일 하나면 `raw`, 아니면 `zip`
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// 파일 하나 그대로(`Content-Length`)
    #[serde(rename = "raw")]
    Raw,
    /// zip(deflate, 필요하면 zip64). 심볼릭 링크·특수 파일은 넣지 않고 보고서에 적는다.
    #[serde(rename = "zip")]
    Zip,
    /// tar.gz(pax — 모드·소유자·시각·링크 보존)
    #[serde(rename = "tar.gz")]
    TarGz,
}

impl DownloadFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            DownloadFormat::Auto => "auto",
            DownloadFormat::Raw => "raw",
            DownloadFormat::Zip => "zip",
            DownloadFormat::TarGz => "tar.gz",
        }
    }
}

/// 내려받기 링크 요청(`POST /file-restore/sessions/{id}/download-link`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct DownloadLinkRequest {
    pub volume: String,
    /// 고른 항목(같은 볼륨, 1~1000개). 볼륨 루트(빈 경로)는 그 아래 모두
    pub paths: Vec<EntryPath>,
    #[serde(default)]
    pub format: DownloadFormat,
    /// 내려받을 파일 이름(확장자 포함). 없으면 `<백업 이름>-<마지막 조각>.<형식>`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub name: Option<String>,
}

/// 내려받기 요청의 항목 수 상한
pub const MAX_DOWNLOAD_PATHS: usize = 1000;
/// 내려받기 이름의 최대 길이(바이트)
pub const MAX_DOWNLOAD_NAME: usize = 255;

/// 내려받기 링크: 60초 안에 한 번, 같은 사용자만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct DownloadLink {
    /// `/api/v1` 아래 경로(`/file-restore/sessions/{id}/download/{token}`)
    pub url: String,
    /// 남은 초
    pub expires_in: u64,
}

/// 세션 열기의 답: 세션과 그 Task(단계를 Task watch로 볼 수 있다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FileRestoreOpened {
    pub session: FileRestoreSessionView,
    pub task: Uuid,
}

/// `Content-Disposition` 값(RFC 6266): ASCII 대체 `filename="…"`(ASCII 아닌 글자·따옴표·`\`·`/`·제어 문자는 `_`)과
/// `filename*=UTF-8''…`(퍼센트 부호화). 이름이 비면 `download`.
pub fn content_disposition(name: &str) -> String {
    let name = if name.is_empty() { "download" } else { name };
    let fallback: String = name
        .chars()
        .map(|c| match c {
            ' '..='~' if !matches!(c, '"' | '\\' | '/') => c,
            _ => '_',
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn part(s: &str) -> PathPart {
        PathPart::new(s).unwrap()
    }

    #[test]
    fn entry_path_parses_strings() {
        let cases: [(&str, Result<Vec<&str>, PathError>); 11] = [
            ("/", Ok(vec![])),
            ("", Ok(vec![])),
            ("/etc/nginx", Ok(vec!["etc", "nginx"])),
            ("etc/nginx", Ok(vec!["etc", "nginx"])),
            ("/a b/ü.txt", Ok(vec!["a b", "ü.txt"])),
            ("/etc//nginx", Err(PathError::Empty)),
            ("/etc/", Err(PathError::Empty)),
            ("//", Err(PathError::Empty)),
            ("/etc/../shadow", Err(PathError::Dot)),
            ("/./etc", Err(PathError::Dot)),
            ("/a\0b", Err(PathError::Separator)),
        ];
        for (s, want) in cases {
            let got = EntryPath::parse(s);
            match want {
                Ok(parts) => {
                    let parts: Vec<_> = parts.into_iter().map(part).collect();
                    assert_eq!(got, Ok(EntryPath(parts)), "{s:?}");
                }
                Err(e) => assert_eq!(got, Err(e), "{s:?}"),
            }
        }
        let long = format!("/{}", "x".repeat(MAX_NAME_BYTES + 1));
        assert_eq!(EntryPath::parse(&long), Err(PathError::LongName));
        assert!(EntryPath::parse(&format!("/{}", "x".repeat(MAX_NAME_BYTES))).is_ok());
        let deep = "/a".repeat(MAX_PATH_DEPTH + 1);
        assert_eq!(EntryPath::parse(&deep), Err(PathError::Deep));
        assert!(EntryPath::parse(&"/a".repeat(MAX_PATH_DEPTH)).is_ok());
    }

    #[test]
    fn entry_path_json_keeps_non_utf8_names() {
        let raw = PathPart::new(vec![b'c', 0xff, b'f']).unwrap();
        let path = EntryPath(vec![part("etc"), raw.clone()]);
        let json = serde_json::to_value(&path).unwrap();
        assert_eq!(json, json!(["etc", { "b64": "Y/9m" }]));
        assert_eq!(serde_json::from_value::<EntryPath>(json).unwrap(), path);
        assert_eq!(path.display(), "/etc/c\u{fffd}f");
        assert_eq!(path.last(), Some(&raw));
        assert_eq!(path.parent(), EntryPath(vec![part("etc")]));
        // 문자열도 받고 목록으로 되쓴다. b64가 UTF-8이면 문자열로 되쓴다.
        let from_str: EntryPath = serde_json::from_value(json!("/etc/nginx")).unwrap();
        assert_eq!(
            serde_json::to_value(&from_str).unwrap(),
            json!(["etc", "nginx"])
        );
        let utf8: EntryPath = serde_json::from_value(json!([{ "b64": "ZXRj" }])).unwrap();
        assert_eq!(serde_json::to_value(&utf8).unwrap(), json!(["etc"]));
        assert_eq!(serde_json::to_value(EntryPath::root()).unwrap(), json!([]));
        for bad in [
            json!(["..", "x"]),
            json!([""]),
            json!(["a/b"]),
            json!([{ "b64": "!!" }]),
            json!([{ "b64": "Lg==" }]),
            json!([{ "b64": "eA==", "x": 1 }]),
            json!("/a//b"),
            json!(3),
        ] {
            assert!(
                serde_json::from_value::<EntryPath>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let deep = json!(vec!["a"; MAX_PATH_DEPTH + 1]);
        assert!(serde_json::from_value::<EntryPath>(deep).is_err());
    }

    #[test]
    fn entry_view_part_round_trips() {
        let mut e = EntryView {
            name: "a.conf".into(),
            raw: None,
            kind: EntryKind::File,
            size: 3,
            mtime: -1,
            mode: 0o644,
            uid: 0,
            gid: 0,
            link_target: None,
        };
        assert_eq!(e.part(), Ok(part("a.conf")));
        e.raw = Some("Y/9m".into());
        assert_eq!(e.part().unwrap().as_bytes(), b"c\xfff");
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            json!({ "name": "a.conf", "raw": "Y/9m", "kind": "file", "size": 3, "mtime": -1, "mode": 0o644,
                    "uid": 0, "gid": 0 })
        );
    }

    #[test]
    fn volume_ids() {
        assert_eq!(disk_volume_id(0), "disk0");
        assert_eq!(partition_volume_id(1, 5), "disk1p5");
        assert_eq!(
            lv_volume_id("rl", "root", "VGU", "LVU").as_deref(),
            Some("lvm/rl/root")
        );
        assert_eq!(
            lv_volume_id("bad name", "root", "aB3d-x", "Ef9-y").as_deref(),
            Some("lvm/aB3d-x/Ef9-y")
        );
        assert_eq!(lv_volume_id("a/b", "c", "", "x"), None);
        for ok in [
            "disk0",
            "disk12",
            "disk0p2",
            "disk3p15",
            "lvm/rl/root",
            "lvm/vg_0/lv.1+x",
        ] {
            assert!(valid_volume_id(ok), "{ok}");
        }
        for bad in [
            "",
            "disk",
            "disk01",
            "disk0p",
            "disk0p02",
            "diskx",
            "disk-1",
            "disk0p1p2",
            "lvm/rl",
            "lvm/rl/root/x",
            "lvm/../root",
            "lvm/-x/root",
            "lvm/r l/root",
            "part0",
        ] {
            assert!(!valid_volume_id(bad), "{bad}");
        }
    }

    #[test]
    fn guest_path_rules() {
        use GuestOsKind::*;
        for ok in ["a.conf", "a b", "über", "x:y", "trail.", "COM1"] {
            assert_eq!(guest_path_ok(Linux, ok), Ok(()), "{ok}");
        }
        for (bad, err) in [
            ("", GuestPathError::Empty),
            ("..", GuestPathError::Empty),
            ("a/b", GuestPathError::Separator),
            ("a\0", GuestPathError::Separator),
        ] {
            assert_eq!(guest_path_ok(Linux, bad), Err(err.clone()), "{bad:?}");
            assert_eq!(guest_path_ok(Windows, bad), Err(err), "{bad:?}");
        }
        assert_eq!(guest_path_ok(Other, "x:y"), Ok(()));
        for ok in [
            "a.conf", "über", "CONSOLE", "COM0", "com10", "LPT", ".hidden",
        ] {
            assert_eq!(guest_path_ok(Windows, ok), Ok(()), "{ok}");
        }
        for bad in [
            "x:y", "a\"b", "a<b", "a>b", "a|b", "a?b", "a*b", "a\\b", "a\tb",
        ] {
            assert_eq!(
                guest_path_ok(Windows, bad),
                Err(GuestPathError::WindowsChar),
                "{bad:?}"
            );
        }
        for bad in ["trail.", "trail "] {
            assert_eq!(
                guest_path_ok(Windows, bad),
                Err(GuestPathError::WindowsTrailing),
                "{bad:?}"
            );
        }
        for bad in [
            "CON",
            "con",
            "nul.txt",
            "Aux.tar.gz",
            "COM1",
            "lpt9.log",
            "PRN ",
        ] {
            let got = guest_path_ok(Windows, bad);
            assert!(
                matches!(
                    got,
                    Err(GuestPathError::WindowsReserved(_) | GuestPathError::WindowsTrailing)
                ),
                "{bad:?}: {got:?}"
            );
        }
        assert_eq!(
            guest_path_ok(Windows, "nul.txt"),
            Err(GuestPathError::WindowsReserved("nul.txt".into()))
        );
    }

    #[test]
    fn guest_target_names() {
        // 2026-11-24 10:15:00 UTC
        let now = 1_795_515_300;
        assert_eq!(stamp(now), "20261124-101500");
        let none = |_: &str| false;
        for c in [
            GuestConflict::Keep,
            GuestConflict::Skip,
            GuestConflict::Overwrite,
        ] {
            assert_eq!(
                guest_target_name("a.conf", c, none, now),
                Ok(GuestTarget::Write("a.conf".into()))
            );
        }
        let only = |n: &str| n == "a.conf";
        assert_eq!(
            guest_target_name("a.conf", GuestConflict::Keep, only, now),
            Ok(GuestTarget::Write("a.conf.restored-20261124-101500".into()))
        );
        assert_eq!(
            guest_target_name("a.conf", GuestConflict::Skip, only, now),
            Ok(GuestTarget::Skip)
        );
        assert_eq!(
            guest_target_name("a.conf", GuestConflict::Overwrite, only, now),
            Ok(GuestTarget::Overwrite)
        );
        let taken = [
            "a.conf",
            "a.conf.restored-20261124-101500",
            "a.conf.restored-20261124-101500-2",
        ];
        assert_eq!(
            guest_target_name("a.conf", GuestConflict::Keep, |n| taken.contains(&n), now),
            Ok(GuestTarget::Write(
                "a.conf.restored-20261124-101500-3".into()
            ))
        );
        assert_eq!(
            guest_target_name("a.conf", GuestConflict::Keep, |_| true, now),
            Err(NoFreeName("a.conf".into()))
        );
        let task = Uuid::nil();
        assert_eq!(
            guest_temp_name("a.conf", task),
            "a.conf.orbivisor-00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn guest_caps_follow_the_enabled_commands() {
        let all: Vec<(String, bool)> = [
            "guest-file-open",
            "guest-file-write",
            "guest-file-flush",
            "guest-file-close",
            "guest-exec",
            "guest-exec-status",
        ]
        .iter()
        .map(|c| (c.to_string(), true))
        .collect();
        let caps = guest_restore_caps(GuestOsKind::Linux, &all);
        assert!(caps.file_write && caps.exec && caps.reason.is_none());
        // RHEL·Rocky의 기본: guest-file-*·guest-exec*가 막힘(목록에 있으나 enabled=false)
        let blocked: Vec<(String, bool)> = all
            .iter()
            .map(|(c, _)| {
                (
                    c.clone(),
                    !c.starts_with("guest-file") && !c.starts_with("guest-exec"),
                )
            })
            .collect();
        let caps = guest_restore_caps(GuestOsKind::Linux, &blocked);
        assert!(!caps.file_write && !caps.exec);
        assert!(caps.reason.unwrap().contains("/etc/sysconfig/qemu-ga"));
        let no_exec: Vec<(String, bool)> = all
            .iter()
            .map(|(c, on)| (c.clone(), *on && c != "guest-exec-status"))
            .collect();
        let caps = guest_restore_caps(GuestOsKind::Windows, &no_exec);
        assert!(caps.file_write && !caps.exec);
        assert!(caps.reason.unwrap().contains("guest-exec"));
        assert_eq!(guest_os_kind(Some("mswindows"), true), GuestOsKind::Windows);
        assert_eq!(guest_os_kind(Some("debian"), true), GuestOsKind::Linux);
        assert_eq!(guest_os_kind(None, true), GuestOsKind::Linux);
        assert_eq!(guest_os_kind(None, false), GuestOsKind::Other);
        let names: Vec<String> = guest_keep_names("a", 0).collect();
        assert_eq!(names.len(), MAX_KEEP_NAMES as usize);
        assert_eq!(names[0], "a.restored-19700101-000000");
        assert_eq!(names[1], "a.restored-19700101-000000-2");
    }

    #[test]
    fn guest_dirs_are_absolute_and_filtered() {
        use GuestOsKind::*;
        let d = GuestDir::parse(Linux, "/srv//restored/").unwrap();
        assert_eq!(d.path(), "/srv/restored");
        assert_eq!(
            d.join(&["etc".into(), "a.conf".into()]).unwrap(),
            "/srv/restored/etc/a.conf"
        );
        assert_eq!(GuestDir::parse(Linux, "/").unwrap().path(), "/");
        assert_eq!(
            GuestDir::parse(Linux, "/")
                .unwrap()
                .join(&["x".into()])
                .unwrap(),
            "/x"
        );
        for bad in ["srv", "", "/srv/../etc", "/srv/./x", "C:\\x"] {
            assert!(GuestDir::parse(Linux, bad).is_err(), "{bad:?}");
        }
        let w = GuestDir::parse(Windows, "c:/Users\\me").unwrap();
        assert_eq!(w.path(), "C:\\Users\\me");
        assert_eq!(w.join(&["a.txt".into()]).unwrap(), "C:\\Users\\me\\a.txt");
        assert_eq!(GuestDir::parse(Windows, "D:").unwrap().path(), "D:\\");
        for bad in [
            "/srv", "C", "1:\\x", "C:x", "C:\\a:b", "C:\\CON", "C:\\dir.",
        ] {
            assert!(GuestDir::parse(Windows, bad).is_err(), "{bad:?}");
        }
        let long = format!("C:\\{}", "a".repeat(300));
        assert!(GuestDir::parse(Windows, &long).is_err());
        let near = GuestDir::parse(Windows, &format!("C:\\{}", "a".repeat(250))).unwrap();
        assert!(near.join(&["b".repeat(5)]).is_ok());
        assert!(near.join(&["b".repeat(10)]).is_err());
    }

    /// 명령은 셸 없이(argv). Windows는 `-EncodedCommand`라 경로가 명령줄로 해석되지 않고, 작은따옴표는 두 번 쓴다.
    #[test]
    fn guest_commands_are_argv_without_a_shell() {
        use GuestOsKind::*;
        assert_eq!(
            guest_mkdir_command(Linux, "/srv/a b"),
            (
                "/bin/mkdir".to_string(),
                vec!["-p".to_string(), "--".into(), "/srv/a b".into()]
            )
        );
        assert_eq!(
            guest_move_command(Other, "/a/x.tmp", "/a/x").1,
            ["-f", "-T", "--", "/a/x.tmp", "/a/x"]
        );
        assert_eq!(
            guest_remove_command(Linux, "/a/-x").1,
            ["-f", "--", "/a/-x"]
        );
        let (exe, args) = guest_move_command(Windows, "C:\\it's $(x).tmp", "C:\\it\u{2019}s");
        assert_eq!(exe, "powershell.exe");
        assert_eq!(
            &args[..3],
            ["-NoProfile", "-NonInteractive", "-EncodedCommand"]
        );
        let raw = STANDARD.decode(&args[3]).unwrap();
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert_eq!(
            script,
            "$ErrorActionPreference='Stop'; Move-Item -Force -LiteralPath 'C:\\it''s $(x).tmp' -Destination 'C:\\it\u{2019}\u{2019}s'"
        );
        let (_, args) = guest_mkdir_command(Windows, "C:\\a");
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn guest_file_ops_round_trip_and_hide_data() {
        let op = GuestFileOp::Write {
            handle: 3,
            data: "c2VjcmV0".into(),
        };
        let json = serde_json::to_value(&op).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"op": "write", "handle": 3, "data": "c2VjcmV0"})
        );
        assert_eq!(serde_json::from_value::<GuestFileOp>(json).unwrap(), op);
        assert!(!format!("{op:?}").contains("c2VjcmV0"));
        let a = GuestFileAnswer::Exited {
            code: 1,
            stderr: "no".into(),
        };
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, r#"{"answer":"exited","code":1,"stderr":"no"}"#);
        assert_eq!(serde_json::from_str::<GuestFileAnswer>(&json).unwrap(), a);
        let r: RestoreToVmRequest = serde_json::from_value(serde_json::json!({
            "volume": "disk0", "paths": ["/etc/hosts"], "vm": "web", "target_dir": "/tmp"
        }))
        .unwrap();
        assert_eq!(r.conflict, GuestConflict::Keep);
    }

    #[test]
    fn content_disposition_has_ascii_fallback() {
        assert_eq!(
            content_disposition("web-20261123-030000-nginx.zip"),
            "attachment; filename=\"web-20261123-030000-nginx.zip\"; filename*=UTF-8''web-20261123-030000-nginx.zip"
        );
        assert_eq!(
            content_disposition("보고서 \"1\".txt"),
            "attachment; filename=\"___ _1_.txt\"; \
             filename*=UTF-8''%EB%B3%B4%EA%B3%A0%EC%84%9C%20%221%22.txt"
        );
        assert_eq!(
            content_disposition("a\\b/c\r\n"),
            "attachment; filename=\"a_b_c__\"; filename*=UTF-8''a%5Cb%2Fc%0D%0A"
        );
        assert_eq!(
            content_disposition(""),
            "attachment; filename=\"download\"; filename*=UTF-8''download"
        );
    }

    fn fr(sessions: u32, accel: &str) -> FileRestoreNode {
        FileRestoreNode {
            image_version: "0.69.0".into(),
            arch: Arch::X86_64,
            accel: accel.into(),
            sessions,
            max_sessions: 4,
        }
    }

    fn cand<'a>(
        id: Uuid,
        name: &'a str,
        fr: Option<&'a FileRestoreNode>,
    ) -> SessionNodeCandidate<'a> {
        SessionNodeCandidate {
            id,
            name,
            online: true,
            reads_backup: true,
            file_restore: fr,
        }
    }

    #[test]
    fn session_node_order() {
        let ids: Vec<Uuid> = (0..3).map(|_| Uuid::now_v7()).collect();
        let (kvm0, kvm1, tcg0, kvm4) = (fr(0, "kvm"), fr(1, "kvm"), fr(0, "tcg"), fr(4, "kvm"));
        let (kvm0, kvm1, tcg0, kvm4) = (Some(&kvm0), Some(&kvm1), Some(&tcg0), Some(&kvm4));
        let other = Uuid::now_v7();
        // API 노드가 먼저(세션이 더 있어도)
        let c = [cand(ids[0], "a", kvm0), cand(ids[1], "b", kvm1)];
        assert_eq!(choose_session_node(&c, ids[1]), Ok(ids[1]));
        // 그다음 세션이 적은 노드
        assert_eq!(choose_session_node(&c, other), Ok(ids[0]));
        // 세션이 같으면 가속이 있는 노드, 그다음 이름
        let c = [
            cand(ids[0], "a", tcg0),
            cand(ids[1], "z", kvm0),
            cand(ids[2], "b", kvm0),
        ];
        assert_eq!(choose_session_node(&c, other), Ok(ids[2]));
        // 꽉 찬 API 노드는 건너뛴다
        let c = [cand(ids[0], "a", kvm4), cand(ids[1], "b", tcg0)];
        assert_eq!(choose_session_node(&c, ids[0]), Ok(ids[1]));
        // 모두 꽉 참
        let c = [cand(ids[0], "a", kvm4)];
        assert_eq!(choose_session_node(&c, other), Err(NoSessionNode::Busy));
        assert_eq!(NoSessionNode::Busy.code(), "file_restore_busy");
        // 이미지가 있는 노드는 백업을 못 읽음
        let mut c = [cand(ids[0], "a", kvm0), cand(ids[1], "b", None)];
        c[0].reads_backup = false;
        assert_eq!(choose_session_node(&c, other), Err(NoSessionNode::NoReader));
        // 오프라인이면 이미지가 없는 셈
        c[0].reads_backup = true;
        c[0].online = false;
        assert_eq!(choose_session_node(&c, other), Err(NoSessionNode::NoImage));
        assert_eq!(choose_session_node(&[], other), Err(NoSessionNode::NoImage));
        assert_eq!(NoSessionNode::NoImage.code(), "file_restore_unavailable");
        assert!(
            !fr(0, "qemu-tcg").accelerated()
                && fr(0, "hvf").accelerated()
                && !fr(0, "?").accelerated()
        );
    }

    #[test]
    fn views_omit_empty_fields() {
        let v = VolumeView {
            id: "disk0p2".into(),
            disk: 0,
            kind: VolumeKind::Partition,
            size: 1 << 30,
            partition: Some(PartitionInfo {
                table: PartitionTable::Gpt,
                number: 2,
                type_name: "Linux filesystem".into(),
                uuid: None,
            }),
            fs: Some("ext4".into()),
            label: None,
            uuid: None,
            mountable: true,
            state: VolumeState::Unmounted,
            note: None,
        };
        let json = serde_json::to_value(&v).unwrap();
        assert_eq!(
            json,
            json!({ "id": "disk0p2", "disk": 0, "kind": "partition", "size": 1u64 << 30,
                    "partition": { "table": "gpt", "number": 2, "type_name": "Linux filesystem" },
                    "fs": "ext4", "mountable": true, "state": "unmounted" })
        );
        assert_eq!(serde_json::from_value::<VolumeView>(json).unwrap(), v);
        let caps = GuestRestoreCaps {
            os: GuestOsKind::Windows,
            file_write: true,
            exec: false,
            reason: None,
        };
        assert_eq!(
            serde_json::to_value(&caps).unwrap(),
            json!({ "os": "windows", "file_write": true, "exec": false })
        );
        assert_eq!(
            serde_json::to_value(GuestConflict::default()).unwrap(),
            json!("keep")
        );
        assert_eq!(
            serde_json::to_value(VolumeKind::LogicalVolume).unwrap(),
            json!("logical_volume")
        );
    }

    #[test]
    fn download_link_requests_take_formats_and_string_paths() {
        let req: DownloadLinkRequest = serde_json::from_value(json!({
            "volume": "disk0p1", "paths": ["/etc/nginx", [{ "b64": "/w==" }]], "format": "tar.gz",
        }))
        .unwrap();
        assert_eq!(req.format, DownloadFormat::TarGz);
        assert_eq!(req.paths[0], EntryPath::parse("/etc/nginx").unwrap());
        assert_eq!(req.paths[1].parts()[0].as_bytes(), [0xff]);
        let auto: DownloadLinkRequest =
            serde_json::from_value(json!({ "volume": "disk0", "paths": ["/"] })).unwrap();
        assert_eq!(auto.format, DownloadFormat::Auto);
        assert!(
            serde_json::from_value::<DownloadLinkRequest>(
                json!({ "volume": "disk0", "paths": [], "format": "rar" })
            )
            .is_err()
        );
        for f in [
            DownloadFormat::Auto,
            DownloadFormat::Raw,
            DownloadFormat::Zip,
            DownloadFormat::TarGz,
        ] {
            assert_eq!(serde_json::to_value(f).unwrap(), json!(f.as_str()));
        }
    }
}
