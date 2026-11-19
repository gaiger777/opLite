//! 게스트 CPU 모델·플래그·노드 CPU 카탈로그·클러스터 기준(계획 63)의 타입과 순수 계산.
//!
//! 모델 목록은 코드에 박지 않는다. 각 노드가 자기 QEMU에 물어 [`CpuCatalogSpec`]으로 알리고, 여기의 함수는 그 카탈로그
//! 위에서 셈한다. 예외는 얼린 `x86-64-v*` 수준 넷([`X86Level`])이다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::node::Node;
use crate::resource::{Kind, Resource, ResourceSpec};
use crate::vm::{Arch, CpuSpec, MachineType, PendingChange, PendingReason, RunningHardware};

/// VM 하나에 둘 수 있는 플래그 수, 클러스터 숨김 목록의 길이 상한
pub const MAX_CPU_FLAGS: usize = 64;

/// `x86-64-v*` 수준의 기반 QEMU 모델(버전 고정). Proxmox는 버전 없는 `qemu64`를 쓰지만 우리는 별칭을 고정한다(§8).
pub const LEVEL_BASE_MODEL: &str = "qemu64-v1";

/// 보류 `cpu`의 설명 끝에 붙는 말: CPUID는 QEMU를 띄울 때만 정해진다.
pub const POWER_CYCLE: &str = "power cycle required; a reboot inside the guest does not apply it";

/// 노드 CPU 카탈로그. 리소스 이름은 [`CpuCatalogSpec::hash`]다.
pub type CpuCatalog = Resource<CpuCatalogSpec, CpuCatalogStatus>;

// ---------------------------------------------------------------------------------------------------------------
// 가속기
// ---------------------------------------------------------------------------------------------------------------

/// QEMU 가속기. 저장되는 칸(`NodeCpu.accel`·`CpuCatalogSpec.accel`)은 문자열이고(모르는 값이 와도 상태를 풀게) 셈할
/// 때 이것으로 읽는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Accel {
    Kvm,
    Hvf,
    Tcg,
}

impl Accel {
    pub fn as_str(self) -> &'static str {
        match self {
            Accel::Kvm => "kvm",
            Accel::Hvf => "hvf",
            Accel::Tcg => "tcg",
        }
    }

    /// `kvm`·`hvf`·`tcg`, 또는 노드의 `hypervisor` 값(`qemu-kvm` …). 모르면 없다.
    pub fn parse(s: &str) -> Option<Self> {
        match s.strip_prefix("qemu-").unwrap_or(s) {
            "kvm" => Some(Accel::Kvm),
            "hvf" => Some(Accel::Hvf),
            "tcg" => Some(Accel::Tcg),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 모델
// ---------------------------------------------------------------------------------------------------------------

/// x86-64 psABI 수준. 정의는 Proxmox VE(`qemu-server` `PVE/QemuServer/CPUConfig.pm`)와 같게 `qemu64` + 플래그이고
/// 얼린다: 바꿔야 하면 새 이름을 만든다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum X86Level {
    V2,
    V2Aes,
    V3,
    V4,
}

const V2_FLAGS: &[&str] = &["popcnt", "pni", "sse4.1", "sse4.2", "ssse3"];
const V2_AES_FLAGS: &[&str] = &["aes", "popcnt", "pni", "sse4.1", "sse4.2", "ssse3"];
const V3_FLAGS: &[&str] = &[
    "aes", "popcnt", "pni", "sse4.1", "sse4.2", "ssse3", "avx", "avx2", "bmi1", "bmi2", "f16c",
    "fma", "abm", "movbe", "xsave",
];
const V4_FLAGS: &[&str] = &[
    "aes", "popcnt", "pni", "sse4.1", "sse4.2", "ssse3", "avx", "avx2", "bmi1", "bmi2", "f16c",
    "fma", "abm", "movbe", "xsave", "avx512f", "avx512bw", "avx512cd", "avx512dq", "avx512vl",
];

impl X86Level {
    /// 높은 것부터
    pub const ALL: [X86Level; 4] = [X86Level::V4, X86Level::V3, X86Level::V2Aes, X86Level::V2];

    pub fn as_str(self) -> &'static str {
        match self {
            X86Level::V2 => "x86-64-v2",
            X86Level::V2Aes => "x86-64-v2-aes",
            X86Level::V3 => "x86-64-v3",
            X86Level::V4 => "x86-64-v4",
        }
    }

    /// [`LEVEL_BASE_MODEL`]에 켜는 플래그(Proxmox의 정의 그대로)
    pub fn flags(self) -> &'static [&'static str] {
        match self {
            X86Level::V2 => V2_FLAGS,
            X86Level::V2Aes => V2_AES_FLAGS,
            X86Level::V3 => V3_FLAGS,
            X86Level::V4 => V4_FLAGS,
        }
    }

    /// 대소문자를 가리지 않는다(Proxmox는 `x86-64-v2-AES`라 쓴다).
    fn parse(s: &str) -> Option<Self> {
        X86Level::ALL
            .into_iter()
            .find(|l| l.as_str().eq_ignore_ascii_case(s))
    }
}

/// 게스트 CPU 모델. JSON은 문자열 하나다(`host`, `max`, `cluster`, `x86-64-v3`, `Icelake-Server-v7`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum CpuModel {
    /// 호스트 CPU를 그대로 노출한다. TCG에서는 max로 대체된다.
    #[default]
    Host,
    /// 이 가속기가 줄 수 있는 모든 것(QEMU 버전에 따라 바뀐다)
    Max,
    /// 켤 때 클러스터 기준([`Baseline`])으로 바뀐다. 기준이 없으면 `host`와 같다.
    Cluster,
    /// x86-64 psABI 수준(x86_64만)
    Level(X86Level),
    /// QEMU 모델 이름 그대로(API가 별칭을 버전 모델로 고정해 저장한다). 존재는 API가 카탈로그로 본다.
    Named(String),
}

impl CpuModel {
    /// QEMU에 주거나 화면에 보일 이름
    pub fn as_str(&self) -> &str {
        match self {
            CpuModel::Host => "host",
            CpuModel::Max => "max",
            CpuModel::Cluster => "cluster",
            CpuModel::Level(l) => l.as_str(),
            CpuModel::Named(n) => n,
        }
    }

    /// `host`·`max`: 노드마다 다른 CPUID로 켜지고 카탈로그 없이도 늘 켤 수 있다.
    pub fn is_host_or_max(&self) -> bool {
        matches!(self, CpuModel::Host | CpuModel::Max)
    }
}

impl fmt::Display for CpuModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for CpuModel {
    type Err = CpuError;

    /// `host`·`max`·`cluster`·수준은 대소문자를 가리지 않는다. 그 밖은 `^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$`이고
    /// `base`(QEMU 내부 모델)는 받지 않는다.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        Ok(match lower.as_str() {
            "host" => CpuModel::Host,
            "max" => CpuModel::Max,
            "cluster" => CpuModel::Cluster,
            "base" => return Err(CpuError::ModelName(s.to_string())),
            _ => match X86Level::parse(s) {
                Some(l) => CpuModel::Level(l),
                None if valid_model_name(s) => CpuModel::Named(s.to_string()),
                None => return Err(CpuError::ModelName(s.to_string())),
            },
        })
    }
}

fn valid_model_name(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

impl TryFrom<String> for CpuModel {
    type Error = CpuError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<CpuModel> for String {
    fn from(m: CpuModel) -> Self {
        m.as_str().to_string()
    }
}

// 이름이 열려 있어 API 문서에서는 문자열이다.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for CpuModel {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some(
                "host, max, cluster, x86-64-v2, x86-64-v2-aes, x86-64-v3, x86-64-v4, or a versioned QEMU model \
                 (e.g. Icelake-Server-v7)",
            ))
            .examples(["host"])
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for CpuModel {}

// ---------------------------------------------------------------------------------------------------------------
// 플래그
// ---------------------------------------------------------------------------------------------------------------

/// VM 플래그 하나: `+aes`(켜기)·`-avx512vbmi`(끄기). 이름은 [`normalize_flag`]로 맞춘다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CpuFlag {
    pub name: String,
    pub on: bool,
}

impl CpuFlag {
    pub fn new(name: &str, on: bool) -> Result<Self, CpuError> {
        Ok(CpuFlag {
            name: normalize_flag(name)?,
            on,
        })
    }
}

impl fmt::Display for CpuFlag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{}", if self.on { '+' } else { '-' }, self.name)
    }
}

// JSON은 문자열(`+aes`·`-avx512vbmi`)이다.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for CpuFlag {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some("'+name' turns a CPU flag on, '-name' hides it"))
            .examples(["-avx512vbmi"])
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for CpuFlag {}

impl FromStr for CpuFlag {
    type Err = CpuError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.as_bytes().first() {
            Some(b'+') => CpuFlag::new(&s[1..], true),
            Some(b'-') => CpuFlag::new(&s[1..], false),
            _ => Err(CpuError::FlagSign(s.to_string())),
        }
    }
}

impl TryFrom<String> for CpuFlag {
    type Error = CpuError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<CpuFlag> for String {
    fn from(f: CpuFlag) -> Self {
        f.to_string()
    }
}

/// 플래그 이름 맞추기: 소문자, `_` → `-`, QEMU의 옛 이름 `sse4-1`·`sse4-2` → `sse4.1`·`sse4.2`(QEMU가 같게 받는
/// 별칭). 문법 `^[a-z0-9][a-z0-9.-]{0,63}$`. 존재는 API가 카탈로그로 본다.
pub fn normalize_flag(name: &str) -> Result<String, CpuError> {
    let n = name.to_ascii_lowercase().replace('_', "-");
    let n = match n.as_str() {
        "sse4-1" => "sse4.1".to_string(),
        "sse4-2" => "sse4.2".to_string(),
        _ => n,
    };
    let b = n.as_bytes();
    let ok = (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-'));
    if ok {
        Ok(n)
    } else {
        Err(CpuError::FlagName(name.to_string()))
    }
}

/// VM 플래그 목록 검사: [`MAX_CPU_FLAGS`]개까지, 같은 이름 두 번(켜고 끄기 동시 포함)은 안 된다.
pub fn validate_flags(flags: &[CpuFlag]) -> Result<(), CpuError> {
    if flags.len() > MAX_CPU_FLAGS {
        return Err(CpuError::TooManyFlags);
    }
    let mut seen = BTreeSet::new();
    for f in flags {
        if !seen.insert(f.name.as_str()) {
            return Err(CpuError::DuplicateFlag(f.name.clone()));
        }
    }
    Ok(())
}

/// 클러스터 숨김 목록을 맞춘다: 이름마다 [`normalize_flag`], 겹침 없이 정렬, [`MAX_CPU_FLAGS`]개까지.
pub fn normalize_mask(mask: &[String]) -> Result<Vec<String>, CpuError> {
    let set = mask
        .iter()
        .map(|m| normalize_flag(m))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if set.len() > MAX_CPU_FLAGS {
        return Err(CpuError::TooManyFlags);
    }
    Ok(set.into_iter().collect())
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CpuError {
    #[error("invalid CPU model name {0:?}")]
    ModelName(String),
    #[error("invalid CPU flag name {0:?}")]
    FlagName(String),
    #[error("CPU flag {0:?} must start with '+' or '-'")]
    FlagSign(String),
    #[error("at most {MAX_CPU_FLAGS} CPU flags")]
    TooManyFlags,
    #[error("CPU flag {0:?} is listed more than once")]
    DuplicateFlag(String),
    #[error("CPU model {model} is not available on {arch:?}")]
    Arch { model: String, arch: Arch },
    #[error("CPU model {0} cannot be pinned as a cluster baseline")]
    PinnedModel(String),
    #[error("cpu.pinned lists {0:?} more than once")]
    DuplicatePinned(Arch),
}

// ---------------------------------------------------------------------------------------------------------------
// 켤 때 해석한 CPU
// ---------------------------------------------------------------------------------------------------------------

/// 해석된 CPU가 어디서 왔나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CpuSource {
    /// VM 스펙의 모델
    #[default]
    Spec,
    /// `cluster` 모델이 클러스터 기준으로 바뀜
    Cluster,
}

/// 켤 때 실제로 쓴 CPU: QEMU `-cpu <model>[,enforce][,<flag>=on|off]…`. `RunningHardware.cpu_resolved`에 기록하고
/// 받는 쪽 마이그레이션은 이것을 그대로 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResolvedCpu {
    /// QEMU `-cpu` 모델 이름(`host`, `max`, `qemu64-v1`, `Icelake-Server-v7` …)
    pub model: String,
    /// 켜고(참) 끈(거짓) 플래그
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub flags: BTreeMap<String, bool>,
    /// 가속기가 못 주는 기능이 있으면 켜지 않는다(명명 모델·수준).
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub enforce: bool,
    #[serde(default)]
    pub source: CpuSource,
}

impl ResolvedCpu {
    /// `Icelake-Server-v7 +aes -avx512vbmi` 모양(플래그는 이름순)
    pub fn describe(&self) -> String {
        let mut s = self.model.clone();
        for (name, on) in &self.flags {
            s.push(' ');
            s.push(if *on { '+' } else { '-' });
            s.push_str(name);
        }
        s
    }

    /// 같은 CPUID로 켜는지(출처는 견주지 않는다)
    pub fn same_cpu(&self, other: &ResolvedCpu) -> bool {
        self.model == other.model && self.flags == other.flags && self.enforce == other.enforce
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// 모델 하나를 기반 `-cpu` 이름·플래그·enforce로 편다(`cluster`는 [`resolve`]가 먼저 바꾼다).
fn expand(
    model: &CpuModel,
    arch: Arch,
    accel: Accel,
) -> Result<(String, BTreeMap<String, bool>, bool), CpuError> {
    Ok(match model {
        CpuModel::Host | CpuModel::Cluster if accel == Accel::Tcg => {
            ("max".into(), BTreeMap::new(), false)
        }
        CpuModel::Host | CpuModel::Cluster => ("host".into(), BTreeMap::new(), false),
        CpuModel::Max => ("max".into(), BTreeMap::new(), false),
        CpuModel::Level(l) => {
            if arch != Arch::X86_64 {
                return Err(CpuError::Arch {
                    model: l.as_str().into(),
                    arch,
                });
            }
            let flags = l.flags().iter().map(|f| (f.to_string(), true)).collect();
            (LEVEL_BASE_MODEL.into(), flags, true)
        }
        CpuModel::Named(n) => (n.clone(), BTreeMap::new(), true),
    })
}

/// 켤 때의 CPU를 정한다(순수). `cpu`는 VM 스펙, `arch`·`accel`은 켜는 노드, `baseline`은 그 아키텍처의 클러스터
/// 기준(없으면 `cluster` = `host`), `mask`는 클러스터 숨김 목록, `catalog`는 켜는 노드의 카탈로그다.
///
/// 순서: 모델 펴기(`host`는 TCG면 `max`, 수준은 `qemu64-v1` + 정의 플래그, `cluster`는 기준 모델·플래그) → 숨김
/// 목록 끄기(카탈로그가 있으면 그 카탈로그가 모르는 이름은 건너뜀 — `sve`는 x86에서 무시. 카탈로그가 없으면 모두 끈다:
/// 숨기기가 목적이라 켜기 실패가 조용히 드러내는 것보다 낫다) → VM 플래그 얹기(숨김 목록에 있는 이름은 숨김이 이긴다).
pub fn resolve(
    cpu: &CpuSpec,
    arch: Arch,
    accel: Accel,
    baseline: Option<&Baseline>,
    mask: &[String],
    catalog: Option<&CpuCatalogSpec>,
) -> Result<ResolvedCpu, CpuError> {
    let (model, mut flags, enforce, source) = match (&cpu.model, baseline) {
        (CpuModel::Cluster, Some(b)) => {
            let base: CpuModel = b.model.parse()?;
            let (model, mut flags, enforce) = expand(&base, arch, accel)?;
            flags.extend(b.flags.iter().map(|(k, v)| (k.clone(), *v)));
            (model, flags, enforce, CpuSource::Cluster)
        }
        (m, _) => {
            let (model, flags, enforce) = expand(m, arch, accel)?;
            (model, flags, enforce, CpuSource::Spec)
        }
    };
    let mut masked = BTreeSet::new();
    for m in mask {
        let Ok(name) = normalize_flag(m) else {
            continue;
        };
        if catalog.is_some_and(|c| c.flag_index(&name).is_none()) {
            continue;
        }
        flags.insert(name.clone(), false);
        masked.insert(name);
    }
    for f in &cpu.flags {
        if !masked.contains(&f.name) {
            flags.insert(f.name.clone(), f.on);
        }
    }
    Ok(ResolvedCpu {
        model,
        flags,
        enforce,
        source,
    })
}

/// 노드가 해석된 CPU를 켤 수 없는 까닭
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{}", self.describe())]
pub struct CpuMismatch {
    pub model: String,
    /// 이 노드의 QEMU가 이 모델을 모른다.
    pub unknown_model: bool,
    /// 끄지 않았는데 가속기가 못 주는 플래그, 켰는데 지원하지 않는 플래그
    pub missing: Vec<String>,
    /// 플래그 아닌 까닭(`type`, aarch64 KVM·HVF의 `accel` …)
    pub other: Vec<String>,
}

impl CpuMismatch {
    fn describe(&self) -> String {
        let mut why = Vec::new();
        if self.unknown_model {
            why.push("unknown to this node's QEMU".to_string());
        }
        if !self.missing.is_empty() {
            why.push(format!("missing {}", self.missing.join(", ")));
        }
        if !self.other.is_empty() {
            why.push(format!("not available: {}", self.other.join(", ")));
        }
        format!("cannot run CPU model {} ({})", self.model, why.join("; "))
    }
}

/// 카탈로그 `catalog`의 노드가 해석된 CPU를 켤 수 있는지(순수). `host`·`max`는 늘 된다. 명명 모델은 카탈로그에
/// 있고, 그 `missing`이 모두 꺼진 플래그 안에 있으며(못 쓰는 까닭이 `avx512vbmi` 하나뿐인 모델은 `-avx512vbmi`로 켤
/// 수 있다), 플래그 아닌 까닭이 없고, 켠 플래그가 모두 `supported`에 있어야 한다. aarch64 KVM·HVF 노드는 명명 모델을
/// 못 켠다.
pub fn can_run(resolved: &ResolvedCpu, catalog: &CpuCatalogSpec) -> Result<(), CpuMismatch> {
    if matches!(resolved.model.as_str(), "host" | "max") {
        return Ok(());
    }
    let mut out = CpuMismatch {
        model: resolved.model.clone(),
        unknown_model: false,
        missing: Vec::new(),
        other: Vec::new(),
    };
    if catalog.arch == Arch::Aarch64 && catalog.accel() != Some(Accel::Tcg) {
        out.other.push("accel".into());
    }
    match catalog.model(&resolved.model) {
        None => out.unknown_model = true,
        Some(m) => {
            for name in catalog.names(&m.missing) {
                if resolved.flags.get(name) != Some(&false) {
                    out.missing.push(name.to_string());
                }
            }
            for o in &m.missing_other {
                if !out.other.contains(o) {
                    out.other.push(o.clone());
                }
            }
        }
    }
    for (name, _) in resolved.flags.iter().filter(|(_, on)| **on) {
        let ok = catalog
            .flag_index(name)
            .is_some_and(|i| catalog.supported.contains(i));
        if !ok && !out.missing.contains(name) {
            out.missing.push(name.clone());
        }
    }
    if out.unknown_model || !out.missing.is_empty() || !out.other.is_empty() {
        Err(out)
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 노드 CPU와 카탈로그
// ---------------------------------------------------------------------------------------------------------------

/// 노드가 알리는 CPU(약 250바이트). 카탈로그 본문은 해시 이름의 [`CpuCatalog`] 리소스다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeCpu {
    /// 카탈로그 리소스 이름(SHA-256 hex). 아직 읽지 못했으면 비어 있다.
    pub catalog: String,
    /// QEMU 버전
    pub qemu: String,
    /// `kvm`·`hvf`·`tcg`
    pub accel: String,
    /// 호스트 CPU 벤더(`GenuineIntel`·`AuthenticAMD`). TCG·aarch64는 없을 수 있다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub vendor: Option<String>,
    /// 호스트 CPU 이름(`model-id`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub model_id: Option<String>,
    /// 읽은 시각(유닉스 초)
    pub probed_at: u64,
    /// 마지막 읽기가 실패한 까닭. 앞 카탈로그는 그대로 둔다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub error: Option<String>,
}

/// 카탈로그 `flags` 순번의 비트 집합. JSON은 바이트(비트 i = 바이트 i/8의 i%8번째, 낮은 비트부터)의 base64url(채움
/// 없음)이고 끝의 0 바이트는 쓰지 않는다(408비트 = 68자).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema), schema(value_type = String))]
#[serde(try_from = "String", into = "String")]
pub struct Bits(Vec<u8>);

impl Bits {
    pub fn from_indices(indices: impl IntoIterator<Item = usize>) -> Self {
        let mut b = Vec::new();
        for i in indices {
            if b.len() <= i / 8 {
                b.resize(i / 8 + 1, 0);
            }
            b[i / 8] |= 1 << (i % 8);
        }
        Bits(b)
    }

    pub fn contains(&self, i: usize) -> bool {
        self.0.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)
    }

    /// 켜진 순번(작은 것부터)
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(n, b)| {
            (0..8)
                .filter(move |k| b & (1 << k) != 0)
                .map(move |k| n * 8 + k)
        })
    }

    pub fn count(&self) -> usize {
        self.0.iter().map(|b| b.count_ones() as usize).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|b| *b == 0)
    }
}

impl TryFrom<String> for Bits {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        let mut b = URL_SAFE_NO_PAD
            .decode(s.as_bytes())
            .map_err(|e| format!("invalid bit set: {e}"))?;
        while b.last() == Some(&0) {
            b.pop();
        }
        Ok(Bits(b))
    }
}

impl From<Bits> for String {
    fn from(mut b: Bits) -> Self {
        while b.0.last() == Some(&0) {
            b.0.pop();
        }
        URL_SAFE_NO_PAD.encode(&b.0)
    }
}

/// 노드 QEMU가 아는 CPU 모델·플래그(새 종류 `cpu-catalog`, 이름 = [`CpuCatalogSpec::hash`]). 같은 QEMU·CPU·
/// 가속기인 노드는 같은 카탈로그를 쓴다. 뒤 버전이 칸을 더할 수 있게 모르는 칸을 막지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CpuCatalogSpec {
    pub arch: Arch,
    /// `kvm`·`hvf`·`tcg`
    pub accel: String,
    /// QEMU 버전
    pub qemu: String,
    /// 물어본 머신 유형(VM과 같은 `q35`·`virt`)
    pub machine: MachineType,
    /// QEMU가 아는 플래그 이름([`normalize_flag`]로 맞춤, 정렬). [`Bits`]의 순번이다.
    pub flags: Vec<String>,
    /// 이 가속기가 줄 수 있는 플래그(`full max` 확장의 참)
    pub supported: Bits,
    /// `max`의 벤더
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub vendor: Option<String>,
    /// 모델(이름순)
    pub models: Vec<CatalogModel>,
}

/// 카탈로그의 모델 하나(`query-cpu-definitions` 항목과 그 `static` 확장)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CatalogModel {
    pub name: String,
    /// 별칭이면 가리키는 버전 모델(이 머신 유형에서)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub alias_of: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub vendor: Option<String>,
    /// 이 가속기에서 켤 수 없는 플래그(`unavailable-features`)
    #[serde(default, skip_serializing_if = "Bits::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub missing: Bits,
    /// `unavailable-features` 가운데 플래그가 아닌 것(`type` …, aarch64 KVM·HVF의 `accel`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub missing_other: Vec<String>,
    /// x86 `static` 확장의 참인 플래그. 없으면(aarch64) 모른다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub features: Option<Bits>,
    #[serde(default)]
    pub migration_safe: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub deprecated: bool,
}

/// 카탈로그의 상태(없음)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CpuCatalogStatus {}

impl ResourceSpec for CpuCatalogSpec {
    const KIND: Kind = Kind::CpuCatalog;
    type Status = CpuCatalogStatus;
}

impl CpuCatalogSpec {
    pub fn accel(&self) -> Option<Accel> {
        Accel::parse(&self.accel)
    }

    /// 플래그 이름의 순번(`flags`가 정렬돼 있어야 한다)
    pub fn flag_index(&self, name: &str) -> Option<usize> {
        self.flags.binary_search_by(|f| f.as_str().cmp(name)).ok()
    }

    /// 비트 집합의 플래그 이름(범위 밖 순번은 버린다)
    pub fn names<'a>(&'a self, bits: &'a Bits) -> impl Iterator<Item = &'a str> + 'a {
        bits.indices()
            .filter_map(|i| self.flags.get(i).map(String::as_str))
    }

    pub fn model(&self, name: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.name == name)
    }

    /// 리소스 이름: 정규 JSON(이 타입의 직렬화, 칸 순서 고정)의 SHA-256 hex. 읽은 시각은 `NodeCpu`에 있어 들지 않는다.
    pub fn hash(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let digest = ring::digest::digest(&ring::digest::SHA256, &json);
        data_encoding::HEXLOWER.encode(digest.as_ref())
    }

    /// 모델 `features`(없으면 빈 것)의 플래그 이름
    fn model_features(&self, model: &str) -> BTreeSet<&str> {
        self.model(model)
            .and_then(|m| m.features.as_ref())
            .map(|b| self.names(b).collect())
            .unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 설정과 클러스터 기준
// ---------------------------------------------------------------------------------------------------------------

/// 클러스터 기준 자동 셈
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BaselineMode {
    /// 셈하지 않는다(`pinned`에 있는 아키텍처만 기준이 있다).
    #[default]
    Off,
    /// 리더가 모든 노드가 켤 수 있는 가장 좋은 모델을 셈한다(x86_64만).
    Auto,
}

/// 아키텍처 하나의 고정 기준
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct PinnedBaseline {
    pub arch: Arch,
    /// `cluster`·`host`·`max`는 안 된다.
    pub model: CpuModel,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub flags: Vec<CpuFlag>,
}

/// 설정 `cpu`(계획 63)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct CpuSettings {
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub baseline: BaselineMode,
    /// 아키텍처마다 고정한 기준. 있는 아키텍처는 auto 대신 이것이다(`baseline`이 off여도).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub pinned: Vec<PinnedBaseline>,
    /// auto 기준에 모든 노드가 주는 플래그를 더한다.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub auto_flags: bool,
    /// 모든 VM(`host` 포함)에서 숨길 플래그(API가 `normalize_mask`로 맞춰 저장)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub mask: Vec<String>,
}

impl Default for CpuSettings {
    fn default() -> Self {
        CpuSettings {
            baseline: BaselineMode::Off,
            pinned: Vec::new(),
            auto_flags: true,
            mask: Vec::new(),
        }
    }
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl CpuSettings {
    /// 고정 모델(`cluster`·`host`·`max` 안 됨, 수준은 x86_64만, 아키텍처마다 하나)·플래그·숨김 목록(맞춘 이름, 겹침
    /// 없이 64개까지)을 본다. 모델·플래그의 존재는 API가 카탈로그로 본다.
    pub fn validate(&self) -> Result<(), CpuError> {
        let mut arches = Vec::new();
        for p in &self.pinned {
            if arches.contains(&p.arch) {
                return Err(CpuError::DuplicatePinned(p.arch));
            }
            arches.push(p.arch);
            match &p.model {
                CpuModel::Host | CpuModel::Max | CpuModel::Cluster => {
                    return Err(CpuError::PinnedModel(p.model.to_string()));
                }
                CpuModel::Level(l) if p.arch != Arch::X86_64 => {
                    return Err(CpuError::Arch {
                        model: l.as_str().into(),
                        arch: p.arch,
                    });
                }
                _ => {}
            }
            validate_flags(&p.flags)?;
        }
        if self.mask.len() > MAX_CPU_FLAGS {
            return Err(CpuError::TooManyFlags);
        }
        let mut seen = BTreeSet::new();
        for m in &self.mask {
            if normalize_flag(m).as_ref() != Ok(m) {
                return Err(CpuError::FlagName(m.clone()));
            }
            if !seen.insert(m) {
                return Err(CpuError::DuplicateFlag(m.clone()));
            }
        }
        Ok(())
    }

    pub fn pinned_for(&self, arch: Arch) -> Option<&PinnedBaseline> {
        self.pinned.iter().find(|p| p.arch == arch)
    }
}

/// 기준이 어떻게 정해졌나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum BaselineSource {
    Auto,
    Pinned,
}

/// 바뀌기 전 기준
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PreviousBaseline {
    pub model: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub flags: BTreeMap<String, bool>,
}

/// 아키텍처 하나의 클러스터 기준(`ClusterStatus.cpu_baselines`, 리더가 쓴다). `cluster` 모델 VM이 켤 때 쓴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Baseline {
    /// [`CpuModel`] 이름(버전 모델, `x86-64-v*`, `qemu64-v1`)
    pub model: String,
    /// 모델 위에 켜고 끈 플래그(auto 플래그·고정 플래그·숨김 목록)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub flags: BTreeMap<String, bool>,
    pub mode: BaselineSource,
    /// 셈에 든 노드
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: Vec<Uuid>,
    /// 기준을 낮춘 노드(auto: 빼면 더 좋은 모델이 됨, pinned: 이 기준을 켤 수 없음)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub limited_by: Vec<Uuid>,
    /// 이 모델·플래그가 된 시각(유닉스 초, 리더가 채운다)
    #[serde(default)]
    pub since: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub previous: Option<PreviousBaseline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub warnings: Vec<String>,
}

impl Baseline {
    /// 같은 CPU(모델·플래그)인지. 리더는 다를 때만 다시 쓴다.
    pub fn same_cpu(&self, other: &Baseline) -> bool {
        self.model == other.model && self.flags == other.flags
    }

    /// `Skylake-Server-v5 +pcid -avx512vbmi` 모양
    pub fn describe(&self) -> String {
        ResolvedCpu {
            model: self.model.clone(),
            flags: self.flags.clone(),
            enforce: false,
            source: CpuSource::Cluster,
        }
        .describe()
    }
}

/// 기준 셈에 쓰는 노드 하나(카탈로그를 찾은)
struct BaselineNode<'a> {
    id: Uuid,
    name: &'a str,
    vendor: Option<&'a str>,
    catalog: &'a CpuCatalogSpec,
}

/// 기준 후보: 명명 모델(벤더가 모두 같고 KVM일 때만), 수준, `qemu64-v1`
#[derive(Debug, Clone, PartialEq, Eq)]
enum Candidate {
    Named(String),
    Level(X86Level),
    Qemu64,
}

impl Candidate {
    fn model(&self) -> CpuModel {
        match self {
            Candidate::Named(n) => CpuModel::Named(n.clone()),
            Candidate::Level(l) => CpuModel::Level(*l),
            Candidate::Qemu64 => CpuModel::Named(LEVEL_BASE_MODEL.into()),
        }
    }
}

/// 모델·플래그를 노드가 켤 수 있는지(기준을 켜 보는 것과 같다)
fn node_runs(
    model: &CpuModel,
    flags: &BTreeMap<String, bool>,
    node: &BaselineNode<'_>,
) -> Result<(), CpuMismatch> {
    let accel = node.catalog.accel().unwrap_or(Accel::Tcg);
    let (m, mut f, enforce) = expand(model, node.catalog.arch, accel).map_err(|_| CpuMismatch {
        model: model.to_string(),
        unknown_model: true,
        missing: Vec::new(),
        other: Vec::new(),
    })?;
    f.extend(flags.iter().map(|(k, v)| (k.clone(), *v)));
    can_run(
        &ResolvedCpu {
            model: m,
            flags: f,
            enforce,
            source: CpuSource::Cluster,
        },
        node.catalog,
    )
}

/// 클러스터 기준을 셈한다(순수, 리더가 부른다). `catalogs`는 해시 → 카탈로그다. 아키텍처마다 `pinned`가 있으면 그
/// 모델, 없고 `baseline`이 auto이면 x86_64만 셈한다(aarch64는 순위 정보가 없다). 결과의 `since`·`previous`는 비어
/// 있고 리더가 앞 기록과 견주어 채운다.
///
/// auto(x86_64): 위트니스가 아닌 이 아키텍처의 등록된 노드(온라인·유지보수는 따지지 않는다) 가운데 카탈로그가 있는
/// 노드가 대상이다. 후보는 모든 대상이 켤 수 있는 버전 모델(별칭 아님, `migration_safe`, `deprecated` 아님, 모델
/// 벤더 = 모든 노드의 호스트 벤더, 모두 KVM) 가운데 정적 확장 비트가 가장 많은 것(같으면 이름·버전 뒤), 없으면 가장
/// 높은 `x86-64-v*`, 그것도 없으면 `qemu64-v1`이다. `auto_flags`면 (모든 노드의 `supported` 교집합 ∩ 카탈로그 모든
/// 명명 모델 `features`의 합집합) − 고른 모델의 기능을 더한다. 끝으로 숨김 목록을 끈다.
pub fn baseline(
    nodes: &[Node],
    catalogs: &BTreeMap<String, CpuCatalogSpec>,
    settings: &CpuSettings,
) -> BTreeMap<Arch, Baseline> {
    let mut out = BTreeMap::new();
    let mask = normalize_mask(&settings.mask).unwrap_or_default();
    let arches: BTreeSet<Arch> = nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter_map(|n| n.status.arch)
        .collect();
    for arch in arches {
        let mut warnings = Vec::new();
        let mut targets = Vec::new();
        for n in nodes
            .iter()
            .filter(|n| !n.spec.witness && n.status.arch == Some(arch))
        {
            match n
                .status
                .cpu
                .as_ref()
                .and_then(|c| catalogs.get(&c.catalog).map(|cat| (c, cat)))
            {
                Some((c, cat)) => targets.push(BaselineNode {
                    id: n.id,
                    name: &n.name,
                    vendor: c.vendor.as_deref(),
                    catalog: cat,
                }),
                None => warnings.push(format!(
                    "node {:?} has no CPU catalog yet and is left out",
                    n.name
                )),
            }
        }
        let b = if let Some(p) = settings.pinned_for(arch) {
            Some(pinned(p, &targets, &mask, warnings))
        } else if settings.baseline == BaselineMode::Auto
            && arch == Arch::X86_64
            && !targets.is_empty()
        {
            Some(auto(&targets, settings.auto_flags, &mask, warnings))
        } else {
            None
        };
        if let Some(b) = b {
            out.insert(arch, b);
        }
    }
    out
}

fn mask_flags(flags: &mut BTreeMap<String, bool>, mask: &[String], targets: &[BaselineNode<'_>]) {
    for m in mask {
        if targets.is_empty() || targets.iter().any(|t| t.catalog.flag_index(m).is_some()) {
            flags.insert(m.clone(), false);
        }
    }
}

fn pinned(
    p: &PinnedBaseline,
    targets: &[BaselineNode<'_>],
    mask: &[String],
    mut warnings: Vec<String>,
) -> Baseline {
    let mut flags: BTreeMap<String, bool> =
        p.flags.iter().map(|f| (f.name.clone(), f.on)).collect();
    mask_flags(&mut flags, mask, targets);
    let mut limited_by = Vec::new();
    for t in targets {
        if let Err(e) = node_runs(&p.model, &flags, t) {
            limited_by.push(t.id);
            warnings.push(format!("node {:?} {e}", t.name));
        }
    }
    Baseline {
        model: p.model.to_string(),
        flags,
        mode: BaselineSource::Pinned,
        nodes: targets.iter().map(|t| t.id).collect(),
        limited_by,
        since: 0,
        previous: None,
        warnings,
    }
}

/// 이름 뒤 `-vN`을 수로 견주는 열쇠(`-v10`이 `-v9` 뒤)
fn version_key(name: &str) -> (&str, u32) {
    name.rsplit_once("-v")
        .and_then(|(p, v)| v.parse().ok().map(|v| (p, v)))
        .unwrap_or((name, 0))
}

/// 노드 집합 `set`(순번)에서 고를 후보의 순위(작을수록 좋음). `ranked`는 좋은 것부터 늘어선 후보, `runs[i][k]`는 노드
/// i가 후보 k를 켤 수 있는지, `named_ok`는 이 집합에서 명명 모델을 쓸 수 있는지다.
fn pick(set: &[usize], ranked: &[Candidate], runs: &[Vec<bool>], named_ok: bool) -> usize {
    ranked
        .iter()
        .enumerate()
        .position(|(k, c)| {
            (named_ok || !matches!(c, Candidate::Named(_))) && set.iter().all(|&i| runs[i][k])
        })
        .unwrap_or(ranked.len() - 1)
}

fn auto(
    targets: &[BaselineNode<'_>],
    auto_flags: bool,
    mask: &[String],
    warnings: Vec<String>,
) -> Baseline {
    // 후보: 어느 카탈로그에든 있는 쓸 만한 버전 모델(정적 확장 비트가 많은 것부터), 수준(높은 것부터), qemu64-v1
    let mut named: BTreeMap<String, usize> = BTreeMap::new();
    for t in targets {
        for m in &t.catalog.models {
            if m.alias_of.is_none() && m.migration_safe && !m.deprecated && m.vendor.is_some() {
                let count = m.features.as_ref().map_or(0, Bits::count);
                named.entry(m.name.clone()).or_insert(count);
            }
        }
    }
    let mut named: Vec<(String, usize)> = named.into_iter().collect();
    named.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| version_key(&b.0).cmp(&version_key(&a.0)))
    });
    let ranked: Vec<Candidate> = named
        .into_iter()
        .map(|(n, _)| Candidate::Named(n))
        .chain(X86Level::ALL.into_iter().map(Candidate::Level))
        .chain([Candidate::Qemu64])
        .collect();
    let empty = BTreeMap::new();
    let runs: Vec<Vec<bool>> = targets
        .iter()
        .map(|t| {
            ranked
                .iter()
                .map(|c| {
                    let vendor_ok = match c {
                        Candidate::Named(n) => t
                            .catalog
                            .model(n)
                            .is_some_and(|m| m.vendor.is_some() && m.vendor.as_deref() == t.vendor),
                        _ => true,
                    };
                    vendor_ok && node_runs(&c.model(), &empty, t).is_ok()
                })
                .collect()
        })
        .collect();
    // 명명 모델은 모든 노드가 KVM이고 호스트 벤더가 같을 때만
    let named_ok = |set: &[usize]| {
        let first = set.first().and_then(|&i| targets[i].vendor);
        first.is_some()
            && set.iter().all(|&i| {
                targets[i].vendor == first && targets[i].catalog.accel() == Some(Accel::Kvm)
            })
    };
    let all: Vec<usize> = (0..targets.len()).collect();
    let chosen = pick(&all, &ranked, &runs, named_ok(&all));
    let limited_by = if targets.len() > 1 {
        (0..targets.len())
            .filter(|&i| {
                let rest: Vec<usize> = all.iter().copied().filter(|&j| j != i).collect();
                pick(&rest, &ranked, &runs, named_ok(&rest)) < chosen
            })
            .map(|i| targets[i].id)
            .collect()
    } else {
        Vec::new()
    };
    let model = ranked[chosen].model();
    let mut flags = BTreeMap::new();
    if auto_flags {
        let mut common: Option<BTreeSet<&str>> = None;
        let mut used: BTreeSet<&str> = BTreeSet::new();
        for t in targets {
            let sup: BTreeSet<&str> = t.catalog.names(&t.catalog.supported).collect();
            common = Some(match common {
                None => sup,
                Some(c) => c.intersection(&sup).copied().collect(),
            });
            for m in &t.catalog.models {
                if let Some(f) = &m.features {
                    used.extend(t.catalog.names(f));
                }
            }
        }
        // 고른 모델이 이미 켜는 것(수준은 qemu64-v1의 기능 + 정의 플래그)
        let mut have: BTreeSet<String> = BTreeSet::new();
        let base_name = match &ranked[chosen] {
            Candidate::Named(n) => n.as_str(),
            _ => LEVEL_BASE_MODEL,
        };
        for t in targets {
            have.extend(
                t.catalog
                    .model_features(base_name)
                    .into_iter()
                    .map(String::from),
            );
        }
        if let Candidate::Level(l) = &ranked[chosen] {
            have.extend(l.flags().iter().map(|f| f.to_string()));
        }
        for f in common.unwrap_or_default() {
            if used.contains(f) && !have.contains(f) {
                flags.insert(f.to_string(), true);
            }
        }
    }
    mask_flags(&mut flags, mask, targets);
    Baseline {
        model: model.to_string(),
        flags,
        mode: BaselineSource::Auto,
        nodes: targets.iter().map(|t| t.id).collect(),
        limited_by,
        since: 0,
        previous: None,
        warnings,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 보류 `cpu`
// ---------------------------------------------------------------------------------------------------------------

/// `1x4 host`·`1x4 Icelake-Server-v7 -avx512vbmi +aes` 모양(소켓 × 코어 모델 플래그)
pub fn cpu_name(cpu: &CpuSpec) -> String {
    let mut s = format!("{}x{} {}", cpu.sockets, cpu.cores, cpu.model);
    for f in &cpu.flags {
        s.push(' ');
        s.push_str(&f.to_string());
    }
    s
}

/// 두 플래그 표의 차이: 켜거나 끈 것은 `+x`·`-x`, 빠진 것은 `x=default`
fn flag_diff(from: &BTreeMap<String, bool>, to: &BTreeMap<String, bool>) -> Vec<String> {
    let mut out = Vec::new();
    for (name, on) in to {
        if from.get(name) != Some(on) {
            out.push(format!("{}{name}", if *on { '+' } else { '-' }));
        }
    }
    for name in from.keys().filter(|n| !to.contains_key(*n)) {
        out.push(format!("{name}=default"));
    }
    out
}

/// 실행 중 VM의 보류 `cpu`(순수). 스펙의 토폴로지(소켓)·모델·플래그가 켤 때의 기록(`hw.cpu`)과 다르면 그것이다.
/// 같아도 지금 해석(`wanted`, 노드 검사 루프가 지금 기준·숨김 목록으로 셈함)이 켤 때 해석(`hw.cpu_resolved`)과 다르면
/// 보류다(기준·숨김 목록이 바뀜). 기록이나 `wanted`가 없으면(계획 63 전 기록, 핫플러그 Task) 스펙만 견준다. 이유는
/// 늘 `Restart`(= 전원 껐다 켜기)이고 설명은 바뀐 것과 [`POWER_CYCLE`]이다.
pub fn pending_change(
    spec: &CpuSpec,
    hw: &RunningHardware,
    wanted: Option<&ResolvedCpu>,
) -> Option<PendingChange> {
    let as_map = |flags: &[crate::cpu::CpuFlag]| -> BTreeMap<String, bool> {
        flags.iter().map(|f| (f.name.clone(), f.on)).collect()
    };
    let (spec_flags, hw_flags) = (as_map(&spec.flags), as_map(&hw.cpu.flags));
    if spec.sockets != hw.cpu.sockets || spec.model != hw.cpu.model || spec_flags != hw_flags {
        let mut what = Vec::new();
        if spec.model != hw.cpu.model {
            what.push(format!("{} → {}", hw.cpu.model, spec.model));
        }
        what.extend(flag_diff(&hw_flags, &spec_flags));
        if spec.sockets != hw.cpu.sockets {
            what.push(format!("{} → {} sockets", hw.cpu.sockets, spec.sockets));
        }
        return Some(PendingChange {
            field: "cpu".into(),
            running: cpu_name(&hw.cpu),
            wanted: cpu_name(spec),
            reason: PendingReason::Restart,
            detail: Some(format!("{}; {POWER_CYCLE}", what.join(", "))),
        });
    }
    let (Some(now), Some(want)) = (hw.cpu_resolved.as_ref(), wanted) else {
        return None;
    };
    if now.same_cpu(want) {
        return None;
    }
    let mut what = Vec::new();
    if now.model != want.model {
        let why = if want.source == CpuSource::Cluster || now.source == CpuSource::Cluster {
            " (cluster baseline changed)"
        } else {
            ""
        };
        what.push(format!("{} → {}{why}", now.model, want.model));
    }
    what.extend(flag_diff(&now.flags, &want.flags));
    if now.enforce != want.enforce {
        what.push(format!("enforce={}", want.enforce));
    }
    Some(PendingChange {
        field: "cpu".into(),
        running: now.describe(),
        wanted: want.describe(),
        reason: PendingReason::Restart,
        detail: Some(format!("{}; {POWER_CYCLE}", what.join(", "))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ClusterStatus;
    use crate::node::{NodeSpec, NodeStatus};
    use crate::settings::SettingsSpec;
    use crate::vm::{Firmware, VmSpec};

    fn flag(s: &str) -> CpuFlag {
        s.parse().unwrap()
    }

    fn cpu(model: &str, flags: &[&str]) -> CpuSpec {
        CpuSpec {
            sockets: 1,
            cores: 2,
            model: model.parse().unwrap(),
            max_vcpus: None,
            flags: flags.iter().map(|f| flag(f)).collect(),
        }
    }

    fn map(flags: &[(&str, bool)]) -> BTreeMap<String, bool> {
        flags.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    /// 시험 카탈로그: 플래그 목록(정렬해 순번을 정함), `supported`, 모델(이름, 벤더, missing, features)
    struct M<'a> {
        name: &'a str,
        vendor: Option<&'a str>,
        missing: &'a [&'a str],
        features: &'a [&'a str],
    }

    fn m<'a>(name: &'a str, missing: &'a [&'a str], features: &'a [&'a str]) -> M<'a> {
        M {
            name,
            vendor: Some("GenuineIntel"),
            missing,
            features,
        }
    }

    const ALL_FLAGS: &[&str] = &[
        "abm",
        "aes",
        "avx",
        "avx2",
        "avx512bw",
        "avx512cd",
        "avx512dq",
        "avx512f",
        "avx512vbmi",
        "avx512vl",
        "bmi1",
        "bmi2",
        "f16c",
        "fma",
        "gfni",
        "movbe",
        "pcid",
        "pni",
        "popcnt",
        "sse4.1",
        "sse4.2",
        "ssse3",
        "sve",
        "xsave",
    ];

    fn catalog(arch: Arch, accel: &str, supported: &[&str], models: &[M<'_>]) -> CpuCatalogSpec {
        let mut flags: Vec<String> = ALL_FLAGS.iter().map(|f| f.to_string()).collect();
        flags.sort();
        let bits = |names: &[&str]| {
            Bits::from_indices(
                names
                    .iter()
                    .map(|n| flags.binary_search_by(|f| f.as_str().cmp(n)).unwrap()),
            )
        };
        CpuCatalogSpec {
            arch,
            accel: accel.into(),
            qemu: "11.1.2".into(),
            machine: arch.machine(),
            supported: bits(supported),
            vendor: Some("GenuineIntel".into()),
            models: models
                .iter()
                .map(|m| CatalogModel {
                    name: m.name.into(),
                    alias_of: None,
                    vendor: m.vendor.map(String::from),
                    missing: bits(m.missing),
                    missing_other: Vec::new(),
                    features: Some(bits(m.features)),
                    migration_safe: true,
                    deprecated: false,
                })
                .collect(),
            flags,
        }
    }

    const V3ISH: &[&str] = &[
        "abm", "aes", "avx", "avx2", "bmi1", "bmi2", "f16c", "fma", "movbe", "pni", "popcnt",
        "sse4.1", "sse4.2", "ssse3", "xsave",
    ];
    const V4ISH: &[&str] = &[
        "abm", "aes", "avx", "avx2", "avx512bw", "avx512cd", "avx512dq", "avx512f", "avx512vl",
        "bmi1", "bmi2", "f16c", "fma", "movbe", "pni", "popcnt", "sse4.1", "sse4.2", "ssse3",
        "xsave",
    ];

    /// 새 KVM Intel 노드: Icelake까지 켬, avx512vbmi·gfni·pcid 지원
    fn new_kvm() -> CpuCatalogSpec {
        let mut sup: Vec<&str> = V4ISH.to_vec();
        sup.extend(["avx512vbmi", "gfni", "pcid"]);
        catalog(
            Arch::X86_64,
            "kvm",
            &sup,
            &[
                m("Haswell-v4", &[], V3ISH),
                m(
                    "Icelake-Server-v7",
                    &[],
                    &[V4ISH, &["avx512vbmi", "gfni"]].concat(),
                ),
                m("Skylake-Server-v5", &[], V4ISH),
                m("qemu64-v1", &[], &["pni"]),
            ],
        )
    }

    /// 옛 KVM Intel 노드: avx512 없음 → Haswell까지
    fn old_kvm() -> CpuCatalogSpec {
        catalog(
            Arch::X86_64,
            "kvm",
            &[V3ISH, &["pcid"]].concat(),
            &[
                m("Haswell-v4", &[], V3ISH),
                m(
                    "Icelake-Server-v7",
                    &["avx512f", "avx512vbmi", "gfni"],
                    &[V4ISH, &["avx512vbmi", "gfni"]].concat(),
                ),
                m("Skylake-Server-v5", &["avx512f"], V4ISH),
                m("qemu64-v1", &[], &["pni"]),
            ],
        )
    }

    fn node(name: &str, n: u128, cat: Option<&CpuCatalogSpec>, vendor: &str) -> Node {
        let mut node = Node::new(
            name,
            NodeSpec {
                address: format!("{name}:8006"),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        node.id = Uuid::from_u128(n);
        node.status = NodeStatus {
            arch: Some(cat.map_or(Arch::X86_64, |c| c.arch)),
            cpu: cat.map(|c| NodeCpu {
                catalog: c.hash(),
                qemu: c.qemu.clone(),
                accel: c.accel.clone(),
                vendor: Some(vendor.into()),
                ..NodeCpu::default()
            }),
            ..NodeStatus::default()
        };
        node
    }

    fn catalogs(list: &[&CpuCatalogSpec]) -> BTreeMap<String, CpuCatalogSpec> {
        list.iter().map(|c| (c.hash(), (*c).clone())).collect()
    }

    #[test]
    fn models_are_one_string_and_round_trip() {
        for (s, m) in [
            ("host", CpuModel::Host),
            ("max", CpuModel::Max),
            ("cluster", CpuModel::Cluster),
            ("x86-64-v2", CpuModel::Level(X86Level::V2)),
            ("x86-64-v2-aes", CpuModel::Level(X86Level::V2Aes)),
            ("x86-64-v3", CpuModel::Level(X86Level::V3)),
            ("x86-64-v4", CpuModel::Level(X86Level::V4)),
            (
                "Icelake-Server-v7",
                CpuModel::Named("Icelake-Server-v7".into()),
            ),
            ("neoverse-n1", CpuModel::Named("neoverse-n1".into())),
            ("qemu64-v1", CpuModel::Named("qemu64-v1".into())),
        ] {
            assert_eq!(serde_json::to_value(&m).unwrap(), s);
            assert_eq!(serde_json::from_value::<CpuModel>(s.into()).unwrap(), m);
        }
        // 낱말·수준은 대소문자를 가리지 않고 소문자로 쓴다. 명명 모델은 표기 그대로다.
        assert_eq!("HOST".parse::<CpuModel>().unwrap(), CpuModel::Host);
        assert_eq!(
            "x86-64-v2-AES".parse::<CpuModel>().unwrap().to_string(),
            "x86-64-v2-aes"
        );
        assert_eq!(CpuModel::default(), CpuModel::Host);
        for bad in ["", "base", "Base", "-x", "a b", "a/b", &"a".repeat(65)] {
            assert!(bad.parse::<CpuModel>().is_err(), "{bad:?}");
            assert!(serde_json::from_value::<CpuModel>(bad.into()).is_err());
        }
        assert!("a".repeat(64).parse::<CpuModel>().is_ok());
    }

    /// Proxmox VE `qemu-server` `src/PVE/QemuServer/CPUConfig.pm`(13007a1, 2026-11-05)의 정의 그대로다.
    #[test]
    fn levels_match_proxmox() {
        fn pve(s: &str) -> Vec<&str> {
            s.split(';').map(|f| f.trim_start_matches('+')).collect()
        }
        assert_eq!(
            X86Level::V2.flags(),
            pve("+popcnt;+pni;+sse4.1;+sse4.2;+ssse3")
        );
        assert_eq!(
            X86Level::V2Aes.flags(),
            pve("+aes;+popcnt;+pni;+sse4.1;+sse4.2;+ssse3")
        );
        assert_eq!(
            X86Level::V3.flags(),
            pve(
                "+aes;+popcnt;+pni;+sse4.1;+sse4.2;+ssse3;+avx;+avx2;+bmi1;+bmi2;+f16c;+fma;+abm;+movbe;+xsave"
            )
        );
        assert_eq!(
            X86Level::V4.flags(),
            pve(
                "+aes;+popcnt;+pni;+sse4.1;+sse4.2;+ssse3;+avx;+avx2;+bmi1;+bmi2;+f16c;+fma;+abm;+movbe;+xsave;+avx512f;+avx512bw;+avx512cd;+avx512dq;+avx512vl"
            )
        );
        // 수준마다 아래 수준을 모두 담는다.
        for w in X86Level::ALL.windows(2) {
            assert!(
                w[1].flags().iter().all(|f| w[0].flags().contains(f)),
                "{w:?}"
            );
        }
        for l in X86Level::ALL {
            assert!(
                l.flags()
                    .iter()
                    .all(|f| normalize_flag(f).as_deref() == Ok(*f))
            );
        }
    }

    #[test]
    fn flags_are_signed_normalized_names() {
        assert_eq!(
            flag("+aes"),
            CpuFlag {
                name: "aes".into(),
                on: true
            }
        );
        assert_eq!(flag("-AVX512VBMI").to_string(), "-avx512vbmi");
        assert_eq!(flag("+avx512_vpopcntdq").name, "avx512-vpopcntdq");
        assert_eq!(flag("-sse4_1").name, "sse4.1");
        assert_eq!(flag("-SSE4-2").name, "sse4.2");
        assert_eq!(flag("+lahf_lm").name, "lahf-lm");
        assert_eq!(
            serde_json::to_value(flag("-avx512vbmi")).unwrap(),
            "-avx512vbmi"
        );
        for bad in ["aes", "+", "-", "+a b", "+a/b", "+-x", "*aes", ""] {
            assert!(bad.parse::<CpuFlag>().is_err(), "{bad:?}");
        }
        assert_eq!(validate_flags(&[flag("+aes"), flag("-avx")]), Ok(()));
        assert_eq!(
            validate_flags(&[flag("+aes"), flag("-aes")]),
            Err(CpuError::DuplicateFlag("aes".into()))
        );
        assert_eq!(
            validate_flags(&[flag("+aes"), flag("+AES")]),
            Err(CpuError::DuplicateFlag("aes".into()))
        );
        let many: Vec<CpuFlag> = (0..=MAX_CPU_FLAGS)
            .map(|i| flag(&format!("+f{i}")))
            .collect();
        assert_eq!(validate_flags(&many[..MAX_CPU_FLAGS]), Ok(()));
        assert_eq!(validate_flags(&many), Err(CpuError::TooManyFlags));
        assert_eq!(
            normalize_mask(&["AVX512VBMI".into(), "avx512vbmi".into(), "gfni".into()]).unwrap(),
            ["avx512vbmi", "gfni"]
        );
    }

    #[test]
    fn bits_are_compact_base64url() {
        let b = Bits::from_indices([0, 7, 8, 407]);
        assert!(b.contains(0) && b.contains(7) && b.contains(8) && b.contains(407));
        assert!(!b.contains(1) && !b.contains(408) && !b.contains(10_000));
        assert_eq!(b.indices().collect::<Vec<_>>(), [0, 7, 8, 407]);
        assert_eq!(b.count(), 4);
        let s = serde_json::to_value(&b).unwrap();
        assert_eq!(s.as_str().unwrap().len(), 68);
        assert_eq!(serde_json::from_value::<Bits>(s).unwrap(), b);
        assert_eq!(serde_json::to_value(Bits::default()).unwrap(), "");
        assert!(Bits::default().is_empty());
        // 끝의 0 바이트는 쓰지 않고, 읽을 때도 버린다.
        assert_eq!(serde_json::to_value(Bits::from_indices([0])).unwrap(), "AQ");
        assert_eq!(
            serde_json::from_value::<Bits>("AQAA".into()).unwrap(),
            Bits::from_indices([0])
        );
        assert!(serde_json::from_value::<Bits>("!!".into()).is_err());
    }

    #[test]
    fn catalog_hash_is_stable_and_lookups_work() {
        let c = new_kvm();
        assert_eq!(c.hash(), new_kvm().hash());
        assert_eq!(c.hash().len(), 64);
        assert_ne!(c.hash(), old_kvm().hash());
        let back: CpuCatalogSpec =
            serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.hash(), c.hash());
        assert_eq!(c.flag_index("abm"), Some(0));
        assert_eq!(c.flag_index("nope"), None);
        assert_eq!(c.accel(), Some(Accel::Kvm));
        let sky = old_kvm();
        let missing: Vec<&str> = sky
            .names(&sky.model("Skylake-Server-v5").unwrap().missing)
            .collect();
        assert_eq!(missing, ["avx512f"]);
        // 모르는 칸은 막지 않는다(뒤 버전이 더할 수 있게).
        let mut json = serde_json::to_value(&c).unwrap();
        json["later"] = 1.into();
        assert!(serde_json::from_value::<CpuCatalogSpec>(json).is_ok());
    }

    #[test]
    fn accel_parses_names_and_hypervisors() {
        assert_eq!(Accel::parse("kvm"), Some(Accel::Kvm));
        assert_eq!(Accel::parse("qemu-hvf"), Some(Accel::Hvf));
        assert_eq!(Accel::parse("qemu-tcg"), Some(Accel::Tcg));
        assert_eq!(Accel::parse("xen"), None);
        for a in [Accel::Kvm, Accel::Hvf, Accel::Tcg] {
            assert_eq!(Accel::parse(a.as_str()), Some(a));
        }
    }

    #[test]
    fn resolve_expands_models_then_masks_then_vm_flags() {
        let x = Arch::X86_64;
        let r = |c: &CpuSpec, accel, b: Option<&Baseline>, mask: &[&str]| {
            let mask: Vec<String> = mask.iter().map(|s| s.to_string()).collect();
            resolve(c, x, accel, b, &mask, Some(&new_kvm())).unwrap()
        };
        // host·max
        let host = r(&cpu("host", &[]), Accel::Kvm, None, &[]);
        assert_eq!(
            (host.model.as_str(), host.enforce, host.flags.is_empty()),
            ("host", false, true)
        );
        assert_eq!(r(&cpu("host", &[]), Accel::Tcg, None, &[]).model, "max");
        assert_eq!(r(&cpu("max", &[]), Accel::Kvm, None, &[]).model, "max");
        // 숨김 목록은 host에도 걸린다. 카탈로그가 모르는 이름(x86의 sve 대신 없는 이름)은 건너뛴다.
        let masked = r(
            &cpu("host", &[]),
            Accel::Kvm,
            None,
            &["avx512vbmi", "nothere"],
        );
        assert_eq!(masked.flags, map(&[("avx512vbmi", false)]));
        // 수준 → qemu64-v1 + 정의 플래그, enforce
        let v3 = r(&cpu("x86-64-v3", &[]), Accel::Kvm, None, &[]);
        assert_eq!(v3.model, LEVEL_BASE_MODEL);
        assert!(v3.enforce);
        assert_eq!(
            v3.flags.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            X86Level::V3.flags().iter().copied().collect()
        );
        assert!(v3.flags.values().all(|v| *v));
        // 명명 모델 + VM 플래그
        let ice = r(
            &cpu("Icelake-Server-v7", &["-avx512vbmi", "+pcid"]),
            Accel::Kvm,
            None,
            &[],
        );
        assert_eq!(ice.model, "Icelake-Server-v7");
        assert!(ice.enforce);
        assert_eq!(ice.flags, map(&[("avx512vbmi", false), ("pcid", true)]));
        assert_eq!(ice.describe(), "Icelake-Server-v7 -avx512vbmi +pcid");
        assert_eq!(ice.source, CpuSource::Spec);
        // 숨김이 VM의 +flag를 이긴다.
        let win = r(
            &cpu("Icelake-Server-v7", &["+avx512vbmi"]),
            Accel::Kvm,
            None,
            &["avx512vbmi"],
        );
        assert_eq!(win.flags, map(&[("avx512vbmi", false)]));
        // VM 플래그는 수준의 정의 플래그를 덮는다.
        let v2 = r(&cpu("x86-64-v2-aes", &["-aes"]), Accel::Kvm, None, &[]);
        assert_eq!(v2.flags.get("aes"), Some(&false));
        // cluster: 기준이 없으면 host(TCG면 max), 있으면 기준 모델·플래그
        assert_eq!(r(&cpu("cluster", &[]), Accel::Kvm, None, &[]).model, "host");
        assert_eq!(r(&cpu("cluster", &[]), Accel::Tcg, None, &[]).model, "max");
        let b = Baseline {
            model: "Skylake-Server-v5".into(),
            flags: map(&[("pcid", true), ("gfni", false)]),
            mode: BaselineSource::Auto,
            nodes: Vec::new(),
            limited_by: Vec::new(),
            since: 0,
            previous: None,
            warnings: Vec::new(),
        };
        let cl = r(&cpu("cluster", &["+gfni"]), Accel::Kvm, Some(&b), &["pcid"]);
        assert_eq!(cl.model, "Skylake-Server-v5");
        assert_eq!(cl.source, CpuSource::Cluster);
        assert!(cl.enforce);
        assert_eq!(cl.flags, map(&[("gfni", true), ("pcid", false)]));
        // 기준이 수준이면 편다.
        let lb = Baseline {
            model: "x86-64-v2".into(),
            flags: BTreeMap::new(),
            ..b.clone()
        };
        assert_eq!(
            r(&cpu("cluster", &[]), Accel::Kvm, Some(&lb), &[]).model,
            LEVEL_BASE_MODEL
        );
        // 기준은 cluster VM에만 쓰인다.
        assert_eq!(
            r(&cpu("host", &[]), Accel::Kvm, Some(&b), &[]).model,
            "host"
        );
        // 수준은 x86_64만
        assert_eq!(
            resolve(
                &cpu("x86-64-v2", &[]),
                Arch::Aarch64,
                Accel::Tcg,
                None,
                &[],
                None
            ),
            Err(CpuError::Arch {
                model: "x86-64-v2".into(),
                arch: Arch::Aarch64
            })
        );
        // 카탈로그가 없으면 숨김 목록을 모두 끈다.
        let none = resolve(
            &cpu("host", &[]),
            x,
            Accel::Kvm,
            None,
            &["sve".into()],
            None,
        )
        .unwrap();
        assert_eq!(none.flags, map(&[("sve", false)]));
        // aarch64 max에 sve 숨김(카탈로그가 sve를 앎)
        let arm = catalog(Arch::Aarch64, "tcg", &["sve"], &[]);
        let a = resolve(
            &cpu("max", &[]),
            Arch::Aarch64,
            Accel::Tcg,
            None,
            &["sve".into(), "avx512vbmi".into()],
            Some(&arm),
        );
        // 시험 카탈로그는 플래그 이름을 함께 쓰므로 둘 다 안다.
        assert_eq!(a.unwrap().flags.get("sve"), Some(&false));
    }

    #[test]
    fn can_run_checks_models_missing_and_supported_flags() {
        let res = |model: &str, flags: &[(&str, bool)]| ResolvedCpu {
            model: model.into(),
            flags: map(flags),
            enforce: true,
            source: CpuSource::Spec,
        };
        let old = old_kvm();
        assert_eq!(
            can_run(&res("host", &[("avx512vbmi", false)]), &old),
            Ok(())
        );
        assert_eq!(can_run(&res("max", &[]), &old), Ok(()));
        assert_eq!(can_run(&res("Haswell-v4", &[]), &old), Ok(()));
        let e = can_run(&res("Skylake-Server-v5", &[]), &old).unwrap_err();
        assert_eq!(e.missing, ["avx512f"]);
        assert_eq!(
            e.to_string(),
            "cannot run CPU model Skylake-Server-v5 (missing avx512f)"
        );
        // 못 쓰는 까닭을 모두 끄면 켤 수 있다.
        assert_eq!(
            can_run(&res("Skylake-Server-v5", &[("avx512f", false)]), &old),
            Ok(())
        );
        // 켠 플래그는 지원해야 한다.
        let e = can_run(&res("Haswell-v4", &[("gfni", true), ("pcid", true)]), &old).unwrap_err();
        assert_eq!(e.missing, ["gfni"]);
        assert_eq!(
            can_run(&res("Haswell-v4", &[("gfni", true)]), &new_kvm()),
            Ok(())
        );
        // 모르는 모델
        let e = can_run(&res("Zen9-v1", &[]), &old).unwrap_err();
        assert!(e.unknown_model);
        assert!(e.to_string().contains("unknown to this node's QEMU"), "{e}");
        // 플래그 아닌 까닭
        let mut odd = old_kvm();
        odd.models[0].missing_other = vec!["type".into()];
        assert_eq!(
            can_run(&res("Haswell-v4", &[]), &odd).unwrap_err().other,
            ["type"]
        );
        // 수준(qemu64-v1 + 플래그)
        let v4 = resolve(
            &cpu("x86-64-v4", &[]),
            Arch::X86_64,
            Accel::Kvm,
            None,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(can_run(&v4, &new_kvm()), Ok(()));
        let e = can_run(&v4, &old).unwrap_err();
        assert_eq!(
            e.missing,
            ["avx512bw", "avx512cd", "avx512dq", "avx512f", "avx512vl"]
        );
        // aarch64: TCG만 명명 모델을 켠다.
        let arm = |accel| {
            catalog(
                Arch::Aarch64,
                accel,
                &["sve"],
                &[M {
                    name: "neoverse-n1",
                    vendor: None,
                    missing: &[],
                    features: &[],
                }],
            )
        };
        assert_eq!(can_run(&res("neoverse-n1", &[]), &arm("tcg")), Ok(()));
        for accel in ["kvm", "hvf"] {
            let e = can_run(&res("neoverse-n1", &[]), &arm(accel)).unwrap_err();
            assert_eq!(e.other, ["accel"]);
            assert_eq!(can_run(&res("max", &[("sve", false)]), &arm(accel)), Ok(()));
        }
    }

    #[test]
    fn auto_baseline_picks_the_best_common_model() {
        let (new, old) = (new_kvm(), old_kvm());
        let auto = CpuSettings {
            baseline: BaselineMode::Auto,
            auto_flags: false,
            ..CpuSettings::default()
        };
        // 같은 노드 둘 → 정적 확장 비트가 가장 많은 Icelake
        let nodes = [
            node("n1", 1, Some(&new), "GenuineIntel"),
            node("n2", 2, Some(&new), "GenuineIntel"),
        ];
        let b = baseline(&nodes, &catalogs(&[&new]), &auto);
        let x = &b[&Arch::X86_64];
        assert_eq!(x.model, "Icelake-Server-v7");
        assert_eq!(x.mode, BaselineSource::Auto);
        assert_eq!(x.nodes, [Uuid::from_u128(1), Uuid::from_u128(2)]);
        assert!(x.limited_by.is_empty() && x.flags.is_empty() && x.warnings.is_empty());
        // 약한 노드가 들어오면 내리고 그 노드를 적는다.
        let mut nodes = nodes.to_vec();
        nodes.push(node("n3", 3, Some(&old), "GenuineIntel"));
        let b = baseline(&nodes, &catalogs(&[&new, &old]), &auto);
        let x = &b[&Arch::X86_64];
        assert_eq!(x.model, "Haswell-v4");
        assert_eq!(x.limited_by, [Uuid::from_u128(3)]);
        // auto_flags: 공통 supported ∩ 모델이 쓰는 플래그 − Haswell의 기능 = 없음(pcid는 어느 모델도 안 씀)
        let flags = CpuSettings {
            auto_flags: true,
            ..auto.clone()
        };
        let b = baseline(&nodes, &catalogs(&[&new, &old]), &flags);
        assert!(
            b[&Arch::X86_64].flags.is_empty(),
            "{:?}",
            b[&Arch::X86_64].flags
        );
        // 숨김 목록은 기준에도 끈다.
        let masked = CpuSettings {
            mask: vec!["avx2".into()],
            ..auto.clone()
        };
        let b = baseline(&nodes, &catalogs(&[&new, &old]), &masked);
        assert_eq!(b[&Arch::X86_64].flags, map(&[("avx2", false)]));
        // 벤더가 섞이면 명명 모델 없이 가장 높은 수준
        let mut mixed = nodes[..2].to_vec();
        mixed.push(node("amd", 4, Some(&new), "AuthenticAMD"));
        let b = baseline(&mixed, &catalogs(&[&new]), &auto);
        assert_eq!(b[&Arch::X86_64].model, "x86-64-v4");
        assert_eq!(b[&Arch::X86_64].limited_by, [Uuid::from_u128(4)]);
        // TCG 노드가 있어도 명명 모델은 없다.
        let mut tcg = new_kvm();
        tcg.accel = "tcg".into();
        let b = baseline(
            &[node("t", 5, Some(&tcg), "GenuineIntel")],
            &catalogs(&[&tcg]),
            &auto,
        );
        assert_eq!(b[&Arch::X86_64].model, "x86-64-v4");
        // 아무 수준도 못 켜면 qemu64-v1
        let bare = catalog(
            Arch::X86_64,
            "kvm",
            &["pni"],
            &[m("qemu64-v1", &[], &["pni"])],
        );
        let b = baseline(
            &[node("b", 6, Some(&bare), "GenuineIntel")],
            &catalogs(&[&bare]),
            &auto,
        );
        assert_eq!(b[&Arch::X86_64].model, "qemu64-v1");
        // 카탈로그 없는 노드는 빼고 경고한다. 위트니스는 보지 않는다.
        let mut w = node("w", 8, None, "GenuineIntel");
        w.spec.witness = true;
        let b = baseline(
            &[
                node("n1", 1, Some(&new), "GenuineIntel"),
                node("bare", 7, None, "GenuineIntel"),
                w,
            ],
            &catalogs(&[&new]),
            &auto,
        );
        assert_eq!(b[&Arch::X86_64].model, "Icelake-Server-v7");
        assert_eq!(
            b[&Arch::X86_64].warnings,
            ["node \"bare\" has no CPU catalog yet and is left out"]
        );
        assert_eq!(b[&Arch::X86_64].nodes, [Uuid::from_u128(1)]);
        // off면 없다. aarch64 auto도 없다.
        assert!(baseline(&nodes, &catalogs(&[&new, &old]), &CpuSettings::default()).is_empty());
        let arm = catalog(Arch::Aarch64, "tcg", &["sve"], &[]);
        assert!(baseline(&[node("a", 9, Some(&arm), "")], &catalogs(&[&arm]), &auto).is_empty());
    }

    #[test]
    fn auto_flags_add_what_every_node_supports() {
        // 두 노드 모두 Icelake + pcid·gfni 지원, Skylake만 고를 수 있게 Icelake를 못 쓰게 함
        let mut a = new_kvm();
        a.models[1].missing = Bits::from_indices([a.flag_index("avx512vbmi").unwrap()]);
        let settings = CpuSettings {
            baseline: BaselineMode::Auto,
            ..CpuSettings::default()
        };
        let b = baseline(
            &[node("n1", 1, Some(&a), "GenuineIntel")],
            &catalogs(&[&a]),
            &settings,
        );
        let x = &b[&Arch::X86_64];
        assert_eq!(x.model, "Skylake-Server-v5");
        // gfni는 Icelake 정의에 쓰이고 지원됨 → 더함. avx512vbmi도 지원·쓰임 → 더함. pcid는 어느 모델도 안 씀.
        assert_eq!(x.flags, map(&[("avx512vbmi", true), ("gfni", true)]));
    }

    #[test]
    fn pinned_baseline_keeps_its_model_and_names_weak_nodes() {
        let (new, old) = (new_kvm(), old_kvm());
        let settings = CpuSettings {
            pinned: vec![PinnedBaseline {
                arch: Arch::X86_64,
                model: "Skylake-Server-v5".parse().unwrap(),
                flags: vec![flag("+pcid")],
            }],
            mask: vec!["avx512vbmi".into()],
            ..CpuSettings::default()
        };
        assert_eq!(settings.validate(), Ok(()));
        let nodes = [
            node("n1", 1, Some(&new), "GenuineIntel"),
            node("n3", 3, Some(&old), "GenuineIntel"),
        ];
        let b = baseline(&nodes, &catalogs(&[&new, &old]), &settings);
        let x = &b[&Arch::X86_64];
        assert_eq!(
            (x.model.as_str(), x.mode),
            ("Skylake-Server-v5", BaselineSource::Pinned)
        );
        assert_eq!(x.flags, map(&[("avx512vbmi", false), ("pcid", true)]));
        assert_eq!(x.limited_by, [Uuid::from_u128(3)]);
        assert_eq!(
            x.warnings,
            ["node \"n3\" cannot run CPU model Skylake-Server-v5 (missing avx512f)"]
        );
        assert_eq!(x.describe(), "Skylake-Server-v5 -avx512vbmi +pcid");
    }

    #[test]
    fn settings_validate_pinned_models_and_mask() {
        let pin = |arch, model: &str| PinnedBaseline {
            arch,
            model: model.parse().unwrap(),
            flags: Vec::new(),
        };
        let with = |pinned: Vec<PinnedBaseline>, mask: &[&str]| {
            CpuSettings {
                pinned,
                mask: mask.iter().map(|s| s.to_string()).collect(),
                ..CpuSettings::default()
            }
            .validate()
        };
        assert_eq!(
            with(
                vec![pin(Arch::Aarch64, "neoverse-n1")],
                &["avx512vbmi", "sve"]
            ),
            Ok(())
        );
        for m in ["host", "max", "cluster"] {
            assert_eq!(
                with(vec![pin(Arch::X86_64, m)], &[]),
                Err(CpuError::PinnedModel(m.into()))
            );
        }
        assert!(matches!(
            with(vec![pin(Arch::Aarch64, "x86-64-v3")], &[]),
            Err(CpuError::Arch { .. })
        ));
        assert_eq!(
            with(
                vec![
                    pin(Arch::X86_64, "x86-64-v3"),
                    pin(Arch::X86_64, "Haswell-v4")
                ],
                &[]
            ),
            Err(CpuError::DuplicatePinned(Arch::X86_64))
        );
        assert_eq!(
            with(vec![], &["AVX"]),
            Err(CpuError::FlagName("AVX".into()))
        );
        assert_eq!(
            with(vec![], &["avx", "avx"]),
            Err(CpuError::DuplicateFlag("avx".into()))
        );
        assert_eq!(with(vec![], &["avx", "aes"]), Ok(()));
    }

    #[test]
    fn settings_serde_is_backward_compatible() {
        // 없으면 옛 설정 그대로
        let spec = SettingsSpec::default();
        assert!(serde_json::to_value(&spec).unwrap().get("cpu").is_none());
        let json = serde_json::json!({
            "cpu": { "baseline": "auto", "mask": ["avx512vbmi"], "pinned": [{ "arch": "aarch64", "model": "neoverse-n1", "flags": ["-sve"] }] }
        });
        let spec: SettingsSpec = serde_json::from_value(json.clone()).unwrap();
        let cpu = spec.cpu.as_ref().unwrap();
        assert!(cpu.auto_flags);
        assert_eq!(cpu.pinned[0].flags, [flag("-sve")]);
        assert_eq!(serde_json::to_value(&spec).unwrap()["cpu"], json["cpu"]);
        let off = CpuSettings {
            auto_flags: false,
            ..CpuSettings::default()
        };
        assert_eq!(
            serde_json::to_value(&off).unwrap(),
            serde_json::json!({ "baseline": "off", "auto_flags": false })
        );
        assert!(
            serde_json::from_value::<CpuSettings>(serde_json::json!({ "mode": "auto" })).is_err()
        );
    }

    #[test]
    fn status_fields_are_omitted_when_unset() {
        assert!(
            serde_json::to_value(NodeStatus::default())
                .unwrap()
                .get("cpu")
                .is_none()
        );
        let status = NodeStatus {
            cpu: Some(NodeCpu {
                catalog: "ab".repeat(32),
                qemu: "8.2.2".into(),
                accel: "kvm".into(),
                vendor: Some("GenuineIntel".into()),
                model_id: Some("Intel Xeon".into()),
                probed_at: 7,
                error: None,
            }),
            ..NodeStatus::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert!(json["cpu"].get("error").is_none());
        assert_eq!(serde_json::from_value::<NodeStatus>(json).unwrap(), status);

        assert_eq!(
            serde_json::to_string(&ClusterStatus::default()).unwrap(),
            "{}"
        );
        let b = Baseline {
            model: "Haswell-v4".into(),
            flags: BTreeMap::new(),
            mode: BaselineSource::Auto,
            nodes: vec![Uuid::from_u128(1)],
            limited_by: Vec::new(),
            since: 5,
            previous: Some(PreviousBaseline {
                model: "Skylake-Server-v5".into(),
                flags: BTreeMap::new(),
            }),
            warnings: Vec::new(),
        };
        let status = ClusterStatus {
            cpu_baselines: BTreeMap::from([(Arch::X86_64, b)]),
            ..ClusterStatus::default()
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json["cpu_baselines"]["x86_64"]["previous"]["model"],
            "Skylake-Server-v5"
        );
        assert_eq!(
            serde_json::from_value::<ClusterStatus>(json).unwrap(),
            status
        );
    }

    /// 플래그 없는 CPU·해석 기록 없는 하드웨어는 옛 JSON 그대로다.
    #[test]
    fn vm_cpu_serde_is_backward_compatible() {
        let old = serde_json::json!({ "sockets": 1, "cores": 4, "model": "host" });
        let c: CpuSpec = serde_json::from_value(old.clone()).unwrap();
        assert!(c.flags.is_empty());
        assert_eq!(serde_json::to_value(&c).unwrap(), old);
        let c = cpu("Icelake-Server-v7", &["-avx512vbmi", "+aes"]);
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["flags"], serde_json::json!(["-avx512vbmi", "+aes"]));
        assert_eq!(json["model"], "Icelake-Server-v7");
        assert_eq!(serde_json::from_value::<CpuSpec>(json).unwrap(), c);
        assert!(
            serde_json::from_value::<CpuSpec>(serde_json::json!({ "cores": 1, "flags": ["aes"] }))
                .is_err()
        );

        let hw_json = serde_json::json!({
            "cpu": { "sockets": 1, "cores": 2, "model": "max" }, "memory_mib": 512, "firmware": "uefi", "machine": "q35"
        });
        let mut hw: RunningHardware = serde_json::from_value(hw_json.clone()).unwrap();
        assert_eq!(hw.cpu_resolved, None);
        assert_eq!(serde_json::to_value(&hw).unwrap(), hw_json);
        hw.cpu_resolved = Some(ResolvedCpu {
            model: "max".into(),
            flags: BTreeMap::new(),
            enforce: false,
            source: CpuSource::Spec,
        });
        let json = serde_json::to_value(&hw).unwrap();
        assert_eq!(
            json["cpu_resolved"],
            serde_json::json!({ "model": "max", "source": "spec" })
        );
        assert_eq!(serde_json::from_value::<RunningHardware>(json).unwrap(), hw);
    }

    #[test]
    fn vm_validation_checks_flags_and_level_arch() {
        let json = serde_json::json!({
            "cpu": { "cores": 2, "model": "x86-64-v3", "flags": ["-avx2"] }, "memory_mib": 512,
            "machine": "q35", "disks": [], "nics": []
        });
        let mut spec: VmSpec = serde_json::from_value(json).unwrap();
        spec.firmware = Firmware::Uefi;
        assert_eq!(spec.validate(), Ok(()));
        spec.cpu.flags.push(flag("+avx2"));
        assert!(spec.validate().is_err());
        spec.cpu.flags.pop();
        spec.machine = MachineType::Virt;
        assert!(spec.validate().is_err());
        spec.cpu.model = "neoverse-n1".parse().unwrap();
        assert_eq!(spec.validate(), Ok(()));
    }

    fn hw_for(c: &CpuSpec, resolved: Option<ResolvedCpu>) -> RunningHardware {
        let mut hw: RunningHardware = serde_json::from_value(serde_json::json!({
            "cpu": c, "memory_mib": 512, "firmware": "uefi", "machine": "q35"
        }))
        .unwrap();
        hw.cpu_resolved = resolved;
        hw
    }

    #[test]
    fn pending_cpu_compares_spec_then_resolution() {
        let running = cpu("Cascadelake-Server-v5", &[]);
        let now = ResolvedCpu {
            model: "Cascadelake-Server-v5".into(),
            flags: BTreeMap::new(),
            enforce: true,
            source: CpuSource::Cluster,
        };
        let hw = hw_for(&running, Some(now.clone()));
        // 같으면 없다.
        assert_eq!(pending_change(&running, &hw, Some(&now)), None);
        assert_eq!(pending_change(&running, &hw, None), None);
        // 스펙 플래그가 바뀜
        let p = pending_change(&cpu("Cascadelake-Server-v5", &["-avx512vbmi"]), &hw, None).unwrap();
        assert_eq!(
            (p.field.as_str(), p.reason),
            ("cpu", PendingReason::Restart)
        );
        assert_eq!(p.running, "1x2 Cascadelake-Server-v5");
        assert_eq!(p.wanted, "1x2 Cascadelake-Server-v5 -avx512vbmi");
        assert_eq!(p.detail.unwrap(), format!("-avx512vbmi; {POWER_CYCLE}"));
        // 기준이 바뀜(스펙은 같음)
        let want = ResolvedCpu {
            model: "Skylake-Server-v5".into(),
            ..now.clone()
        };
        let p = pending_change(&running, &hw, Some(&want)).unwrap();
        assert_eq!(
            p.detail.unwrap(),
            format!(
                "Cascadelake-Server-v5 → Skylake-Server-v5 (cluster baseline changed); {POWER_CYCLE}"
            )
        );
        assert_eq!(
            (p.running.as_str(), p.wanted.as_str()),
            ("Cascadelake-Server-v5", "Skylake-Server-v5")
        );
        // 숨김 목록이 더해짐
        let want = ResolvedCpu {
            flags: map(&[("avx512vbmi", false)]),
            ..now.clone()
        };
        let p = pending_change(&running, &hw, Some(&want)).unwrap();
        assert_eq!(p.detail.unwrap(), format!("-avx512vbmi; {POWER_CYCLE}"));
        // 숨김 목록이 빠짐
        let hw2 = hw_for(&running, Some(want.clone()));
        let p = pending_change(&running, &hw2, Some(&now)).unwrap();
        assert_eq!(
            p.detail.unwrap(),
            format!("avx512vbmi=default; {POWER_CYCLE}")
        );
        // 계획 63 전 기록(해석 없음)은 스펙만 견준다.
        let legacy = hw_for(&cpu("host", &[]), None);
        assert_eq!(
            pending_change(&cpu("host", &[]), &legacy, Some(&want)),
            None
        );
        let p = pending_change(&cpu("max", &[]), &legacy, None).unwrap();
        assert_eq!(p.detail.unwrap(), format!("host → max; {POWER_CYCLE}"));
    }
}
