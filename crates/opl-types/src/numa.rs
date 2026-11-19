//! NUMA 구조·vNUMA·전용 CPU 고정·hugepages(계획 64 설계 §5·§8): 타입과 순수 계산.
//!
//! 게스트가 보는 모양(vNUMA 노드·페이지 크기)은 [`layout`]이 스펙에서, 호스트에 매인 것(호스트 NUMA 노드·pCPU)은
//! [`fit`]이 노드의 토폴로지와 빈 자원에서 정한다. 이미 켠 VM(마이그레이션·메모리 스냅샷)은 [`fit_fixed`]가 기록된
//! 모양 그대로 호스트 쪽만 고른다. 같은 함수를 스케줄러(어림)·노드 할당기(정확)·상태 머신(합계, [`demand`])이 쓴다.
//! 이 기능을 쓰지 않는 VM은 [`layout`]이 `None`이고 스펙·기록 JSON이 지금과 바이트까지 같다.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cpu::POWER_CYCLE;
use crate::vm::{CpuSpec, PendingChange, PendingReason, RunningHardware, VmSpec};

/// 게스트 NUMA 노드 수의 상한
pub const MAX_NUMA_NODES: u16 = 8;

/// CPU 번호의 상한(`0..=4095`)
pub const MAX_CPU_ID: u16 = 4095;

/// 2M 페이지가 아닌 VM 메모리를 게스트 노드로 나누는 단위(MiB)
const SMALL_UNIT_MIB: u32 = 2;

/// 순열을 모두 보는 상한. 넘으면 욕심쟁이로 고른다(호스트 노드 여럿 × 게스트 노드 여럿).
const MAX_PERMUTATIONS: u64 = 50_000;

// ---------------------------------------------------------------------------------------------------------------
// CPU 집합
// ---------------------------------------------------------------------------------------------------------------

/// CPU 번호 집합. JSON은 Linux cpulist 문자열(`"0-3,8,10-11"`)이고 늘 맞춘 꼴(오름차순·범위 합침)로 쓴다. 빈 집합은
/// `""`이다. 번호는 [`MAX_CPU_ID`]까지.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CpuSet(BTreeSet<u16>);

/// cpulist 문자열을 풀지 못함
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid CPU list {0:?}: use CPU numbers 0-4095 and ranges, e.g. 0-3,8")]
pub struct CpuSetError(pub String);

impl CpuSet {
    pub fn new() -> CpuSet {
        CpuSet::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn contains(&self, cpu: u16) -> bool {
        self.0.contains(&cpu)
    }

    pub fn insert(&mut self, cpu: u16) -> bool {
        self.0.insert(cpu)
    }

    pub fn remove(&mut self, cpu: u16) -> bool {
        self.0.remove(&cpu)
    }

    /// 오름차순
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.0.iter().copied()
    }

    pub fn first(&self) -> Option<u16> {
        self.0.first().copied()
    }

    pub fn union(&self, other: &CpuSet) -> CpuSet {
        CpuSet(self.0.union(&other.0).copied().collect())
    }

    pub fn intersection(&self, other: &CpuSet) -> CpuSet {
        CpuSet(self.0.intersection(&other.0).copied().collect())
    }

    pub fn difference(&self, other: &CpuSet) -> CpuSet {
        CpuSet(self.0.difference(&other.0).copied().collect())
    }

    pub fn is_subset(&self, other: &CpuSet) -> bool {
        self.0.is_subset(&other.0)
    }

    pub fn is_disjoint(&self, other: &CpuSet) -> bool {
        self.0.is_disjoint(&other.0)
    }
}

impl FromIterator<u16> for CpuSet {
    fn from_iter<I: IntoIterator<Item = u16>>(iter: I) -> Self {
        CpuSet(iter.into_iter().collect())
    }
}

impl fmt::Display for CpuSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut it = self.0.iter().copied().peekable();
        let mut first = true;
        while let Some(start) = it.next() {
            let mut end = start;
            while let Some(&next) = it.peek() {
                if end.checked_add(1) != Some(next) {
                    break;
                }
                end = next;
                it.next();
            }
            if !first {
                f.write_str(",")?;
            }
            first = false;
            if start == end {
                write!(f, "{start}")?;
            } else {
                write!(f, "{start}-{end}")?;
            }
        }
        Ok(())
    }
}

impl FromStr for CpuSet {
    type Err = CpuSetError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || CpuSetError(s.to_string());
        let num = |n: &str| -> Result<u16, CpuSetError> {
            n.trim()
                .parse::<u16>()
                .ok()
                .filter(|&n| n <= MAX_CPU_ID)
                .ok_or_else(bad)
        };
        let mut set = BTreeSet::new();
        let text = s.trim();
        if text.is_empty() {
            return Ok(CpuSet(set));
        }
        for part in text.split(',') {
            let (a, b) = match part.split_once('-') {
                Some((a, b)) => (num(a)?, num(b)?),
                None => {
                    let n = num(part)?;
                    (n, n)
                }
            };
            if a > b {
                return Err(bad());
            }
            set.extend(a..=b);
        }
        Ok(CpuSet(set))
    }
}

impl TryFrom<String> for CpuSet {
    type Error = CpuSetError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<CpuSet> for String {
    fn from(s: CpuSet) -> Self {
        s.to_string()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for CpuSet {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, Type};
        ObjectBuilder::new()
            .schema_type(Type::String)
            .description(Some("Linux CPU list: numbers and ranges, e.g. 0-3,8,10-11"))
            .examples(["0-3,8"])
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for CpuSet {}

// ---------------------------------------------------------------------------------------------------------------
// VM 쪽 타입
// ---------------------------------------------------------------------------------------------------------------

/// vCPU 정책(OpenStack `hw:cpu_policy`)
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CpuPolicy {
    /// 노드의 공유 CPU에서 다른 VM과 나눠 돈다(지금과 같다).
    #[default]
    Shared,
    /// vCPU마다 노드의 전용 pCPU 하나를 혼자 쓴다.
    Dedicated,
}

impl CpuPolicy {
    pub fn is_shared(&self) -> bool {
        *self == CpuPolicy::Shared
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CpuPolicy::Shared => "shared",
            CpuPolicy::Dedicated => "dedicated",
        }
    }
}

/// 전용 VM의 SMT 형제 스레드 정책(OpenStack `hw:cpu_thread_policy`)
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum ThreadPolicy {
    /// 형제 스레드를 먼저 채운다.
    #[default]
    Prefer,
    /// vCPU마다 코어 하나를 잡고 형제는 비워 둔다.
    Isolate,
    /// vCPU를 형제 짝으로 놓는다(SMT 호스트만).
    Require,
}

impl ThreadPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadPolicy::Prefer => "prefer",
            ThreadPolicy::Isolate => "isolate",
            ThreadPolicy::Require => "require",
        }
    }
}

/// 전용 VM의 QEMU 에뮬레이터 스레드(메인 루프·I/O·일꾼) 정책(OpenStack `hw:emulator_threads_policy`)
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum EmulatorPolicy {
    /// 노드의 공유 CPU에서 돈다.
    #[default]
    Shared,
    /// 전용 pCPU 하나를 더 잡아 혼자 쓴다.
    Isolate,
}

impl EmulatorPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            EmulatorPolicy::Shared => "shared",
            EmulatorPolicy::Isolate => "isolate",
        }
    }
}

/// 게스트 노드 메모리를 호스트 NUMA 노드에 묶는 방법
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "kebab-case")]
pub enum MemoryPolicy {
    /// 그 호스트 노드에서만 받는다(QEMU `policy=bind`).
    Strict,
    /// 그 호스트 노드를 먼저 쓰고 모자라면 다른 노드에서 받는다(QEMU `policy=preferred`).
    Preferred,
}

impl MemoryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryPolicy::Strict => "strict",
            MemoryPolicy::Preferred => "preferred",
        }
    }
}

/// hugepage 크기 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PageKind {
    #[serde(rename = "2M")]
    Size2M,
    #[serde(rename = "1G")]
    Size1G,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PageKind::Size2M => "2M",
            PageKind::Size1G => "1G",
        }
    }

    /// 페이지 크기(KiB) — `/sys/.../hugepages-<kB>kB`의 숫자
    pub fn size_kib(self) -> u32 {
        match self {
            PageKind::Size2M => 2048,
            PageKind::Size1G => 1024 * 1024,
        }
    }

    /// 페이지 크기(MiB)
    pub fn size_mib(self) -> u32 {
        self.size_kib() / 1024
    }

    /// `/sys`의 페이지 크기(KiB)에 맞는 것. 2M·1G 밖(예: aarch64 64K 커널의 512M)은 None이다.
    pub fn from_kib(kib: u32) -> Option<PageKind> {
        [PageKind::Size2M, PageKind::Size1G]
            .into_iter()
            .find(|k| k.size_kib() == kib)
    }
}

impl fmt::Display for PageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// VM이 원하는 hugepage 크기(`VmSpec.hugepages`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum PageSize {
    #[serde(rename = "2M")]
    Size2M,
    #[serde(rename = "1G")]
    Size1G,
    /// 노드가 가진 가장 큰 것부터(1G, 안 되면 2M). VM 전체가 한 크기다.
    #[serde(rename = "any")]
    Any,
}

impl PageSize {
    pub fn as_str(self) -> &'static str {
        match self {
            PageSize::Size2M => "2M",
            PageSize::Size1G => "1G",
            PageSize::Any => "any",
        }
    }

    /// 시도할 크기(앞부터)
    pub fn kinds(self) -> Vec<PageKind> {
        match self {
            PageSize::Size2M => vec![PageKind::Size2M],
            PageSize::Size1G => vec![PageKind::Size1G],
            PageSize::Any => vec![PageKind::Size1G, PageKind::Size2M],
        }
    }

    /// 이 크기로 켠 기록(`page`)이 이 요구를 채운다.
    pub fn accepts(self, page: PageKind) -> bool {
        self.kinds().contains(&page)
    }
}

impl From<PageKind> for PageSize {
    fn from(k: PageKind) -> Self {
        match k {
            PageKind::Size2M => PageSize::Size2M,
            PageKind::Size1G => PageSize::Size1G,
        }
    }
}

/// VM의 vNUMA(`VmSpec.numa`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NumaSpec {
    /// `"auto"`, 노드 수(1~8), 또는 노드마다 `{cpus, memory_mib}` 목록
    #[serde(default)]
    pub nodes: NumaNodes,
    /// 게스트 노드 메모리를 호스트 NUMA 노드에 묶는다. 주면 공유 VM도 묶는다. 전용·hugepages VM은 주지 않으면
    /// `strict`다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub memory: Option<MemoryPolicy>,
}

/// 게스트 NUMA 노드 나누기
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawNodes", into = "RawNodes")]
pub enum NumaNodes {
    /// 소켓 수의 약수 가운데 노드에 맞는 것(§8)
    #[default]
    Auto,
    /// 노드 수. 소켓 수를 나눠야 한다.
    Count(u16),
    /// 노드마다 vCPU 자리 수와 메모리. 앞 소켓부터 차례로 갖는다.
    List(Vec<NumaNodeSpec>),
}

/// 직접 준 게스트 노드 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NumaNodeSpec {
    /// vCPU 자리 수(핫플러그 상한 기준). 소켓당 자리 수의 배수다.
    pub cpus: u16,
    pub memory_mib: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RawNodes {
    Name(String),
    Count(u16),
    List(Vec<NumaNodeSpec>),
}

impl TryFrom<RawNodes> for NumaNodes {
    type Error = String;
    fn try_from(raw: RawNodes) -> Result<Self, Self::Error> {
        match raw {
            RawNodes::Name(n) if n == "auto" => Ok(NumaNodes::Auto),
            RawNodes::Name(n) => Err(format!(
                "numa.nodes {n:?}: use \"auto\", a node count or a node list"
            )),
            RawNodes::Count(n) => Ok(NumaNodes::Count(n)),
            RawNodes::List(l) => Ok(NumaNodes::List(l)),
        }
    }
}

impl From<NumaNodes> for RawNodes {
    fn from(n: NumaNodes) -> Self {
        match n {
            NumaNodes::Auto => RawNodes::Name("auto".into()),
            NumaNodes::Count(n) => RawNodes::Count(n),
            NumaNodes::List(l) => RawNodes::List(l),
        }
    }
}

// JSON 모양이 enum과 달라서 API 문서 스키마를 손으로 적는다: "auto", 정수, 목록 가운데 하나.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for NumaNodes {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Type};
        OneOfBuilder::new()
            .item(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .enum_values(Some(["auto"])),
            )
            .item(
                ObjectBuilder::new()
                    .schema_type(Type::Integer)
                    .minimum(Some(1))
                    .maximum(Some(MAX_NUMA_NODES)),
            )
            .item(Vec::<NumaNodeSpec>::schema())
            .description(Some(
                "\"auto\", a guest NUMA node count (1-8), or one {cpus, memory_mib} per node",
            ))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for NumaNodes {}

// ---------------------------------------------------------------------------------------------------------------
// 노드 쪽 타입
// ---------------------------------------------------------------------------------------------------------------

/// 노드의 CPU 나누기와 hugepages 예약(`NodeSpec.host_resources`, 설계 §7). 공유 CPU는 `온라인 − 예약 − 전용`이다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HostResources {
    /// 호스트와 Orbivisor의 CPU. VM 스레드는 두지 않는다.
    #[serde(default, skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub reserved_cpus: CpuSet,
    /// 전용 VM의 vCPU와 `isolate` 에뮬레이터만 쓰는 CPU
    #[serde(default, skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub dedicated_cpus: CpuSet,
    /// 실행 중 hugepages 예약(opld가 `nr_hugepages`에 쓴다)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub hugepages: Vec<HugepageReserve>,
}

/// hugepages 예약 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct HugepageReserve {
    pub size: PageKind,
    /// 페이지 수(전체가 이 수가 되게)
    pub count: u32,
    /// 호스트 NUMA 노드. 없으면 커널이 노드에 나눈다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub host_node: Option<u16>,
}

/// 호스트 자원 설정이 노드에 맞지 않음(API 400 `invalid_host_resources`)
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HostResourcesError {
    #[error("{field} {cpus} are not online CPUs of this node")]
    NotOnline { field: &'static str, cpus: CpuSet },
    #[error("reserved_cpus and dedicated_cpus overlap: {0}")]
    Overlap(CpuSet),
    #[error("no shared CPUs left: reserved_cpus and dedicated_cpus take every online CPU")]
    NoShared,
    #[error("hugepages: this node has no host NUMA node {0}")]
    HostNode(u16),
    #[error("hugepages: {0} is listed twice for the same host NUMA node")]
    Duplicate(PageKind),
}

impl HostResources {
    /// 노드 토폴로지에 맞는지 본다: 온라인 CPU의 부분집합, 서로 겹치지 않음, 공유가 비지 않음, 있는 호스트 노드.
    pub fn validate(&self, topology: &NodeTopology) -> Result<(), HostResourcesError> {
        for (field, set) in [
            ("reserved_cpus", &self.reserved_cpus),
            ("dedicated_cpus", &self.dedicated_cpus),
        ] {
            let off = set.difference(&topology.cpus);
            if !off.is_empty() {
                return Err(HostResourcesError::NotOnline { field, cpus: off });
            }
        }
        let both = self.reserved_cpus.intersection(&self.dedicated_cpus);
        if !both.is_empty() {
            return Err(HostResourcesError::Overlap(both));
        }
        if topology
            .cpus
            .difference(&self.reserved_cpus)
            .difference(&self.dedicated_cpus)
            .is_empty()
        {
            return Err(HostResourcesError::NoShared);
        }
        let mut seen = BTreeSet::new();
        for r in &self.hugepages {
            if let Some(n) = r.host_node
                && !topology.nodes.iter().any(|h| h.id == n)
            {
                return Err(HostResourcesError::HostNode(n));
            }
            if !seen.insert((r.size, r.host_node)) {
                return Err(HostResourcesError::Duplicate(r.size));
            }
        }
        Ok(())
    }
}

/// 노드의 CPU·메모리 토폴로지(`NodeStatus.topology`, 설계 §6). 바뀔 때만 쓰므로 빈 자원은 넣지 않는다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NodeTopology {
    pub sockets: u16,
    /// 코어마다 가장 많은 스레드 수(SMT가 없으면 1)
    pub threads_per_core: u8,
    /// 온라인 CPU
    pub cpus: CpuSet,
    /// 호스트 NUMA 노드(id 순)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<HostNode>,
    /// `distances[i][j]`: `nodes[i]`와 `nodes[j]` 사이 거리(`/sys/.../distance`). 비면 모른다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub distances: Vec<Vec<u8>>,
    /// SMT 형제 묶음(스레드가 둘 이상인 코어만)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub siblings: Vec<CpuSet>,
    /// L3 캐시를 나누는 CPU 묶음
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub l3: Vec<CpuSet>,
    /// 커널이 아는 hugepage 크기(KiB)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub page_sizes_kib: Vec<u32>,
    /// 커널 `isolcpus`
    #[serde(skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub isolated: CpuSet,
    /// 커널 `nohz_full`
    #[serde(skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nohz_full: CpuSet,
    /// vCPU 스레드를 pCPU에 고정할 수 있다(Linux).
    pub pinning: bool,
    /// cgroup v2 `cpuset`이 있고 VM 스코프를 쓴다(`AllowedCPUs`를 건다).
    pub cpuset: bool,
    /// 호스트 준비가 덜 된 것(화면에 그대로)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// 탐침 실패(있으면 나머지 칸은 믿지 않는다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
    /// 값이 바뀐 때(유닉스 초)
    pub probed_at: u64,
}

/// 호스트 NUMA 노드 하나
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct HostNode {
    pub id: u16,
    /// 이 노드의 온라인 CPU
    pub cpus: CpuSet,
    /// 노드 메모리 전체(hugepages 풀 포함)
    pub memory_mib: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hugepages: Vec<HugepagePool>,
}

/// 호스트 노드의 hugepages 풀 하나
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HugepagePool {
    pub size_kib: u32,
    /// 예약된 페이지 수(`nr_hugepages`)
    pub total: u32,
}

impl HostNode {
    /// 크기 `kind` 풀의 페이지 수
    pub fn pages(&self, kind: PageKind) -> u64 {
        self.hugepages
            .iter()
            .filter(|p| p.size_kib == kind.size_kib())
            .map(|p| u64::from(p.total))
            .sum()
    }

    /// hugepages 풀 밖의 메모리(MiB) — 보통 VM이 쓸 수 있는 것
    pub fn small_memory_mib(&self) -> u64 {
        let pools: u64 = self
            .hugepages
            .iter()
            .map(|p| u64::from(p.total) * u64::from(p.size_kib) / 1024)
            .sum();
        self.memory_mib.saturating_sub(pools)
    }
}

impl NodeTopology {
    /// 모든 호스트 노드의 크기 `kind` 페이지 수
    pub fn pages(&self, kind: PageKind) -> u64 {
        self.nodes.iter().map(|n| n.pages(kind)).sum()
    }

    /// 호스트 노드 `a`·`b`(`nodes`의 순번) 사이 거리. 모르면 0이다.
    fn distance(&self, a: usize, b: usize) -> u8 {
        self.distances
            .get(a)
            .and_then(|row| row.get(b))
            .copied()
            .unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 켤 때 기록
// ---------------------------------------------------------------------------------------------------------------

/// 켤 때 정한 게스트의 NUMA 모양(`RunningHardware.numa`). 게스트가 보는 것과 마이그레이션 양쪽이 같아야 하는 것이라
/// 라이브 마이그레이션 동안 바꾸지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ResolvedNuma {
    /// 게스트 노드(순번 = QEMU `nodeid`)
    pub nodes: Vec<GuestNode>,
    /// 게스트 노드 사이 거리(`-numa dist`). 비면 QEMU 기본(10·20)이다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub distances: Vec<Vec<u8>>,
    /// hugepage 크기. 없으면 보통 페이지다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub page: Option<PageKind>,
    #[serde(default, skip_serializing_if = "CpuPolicy::is_shared")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub policy: CpuPolicy,
    /// 전용 VM의 스레드 정책(기본을 채운 것). 공유 VM은 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub thread_policy: Option<ThreadPolicy>,
    /// 전용 VM의 에뮬레이터 정책(기본을 채운 것). 공유 VM은 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub emulator: Option<EmulatorPolicy>,
    /// 호스트 노드에 묶는 방법. 없으면 묶지 않는다(공유 vNUMA VM).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub memory: Option<MemoryPolicy>,
}

/// 게스트 NUMA 노드 하나
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct GuestNode {
    /// 이 노드의 소켓(QEMU `socket-id`, 연속)
    pub sockets: Vec<u16>,
    pub memory_mib: u32,
}

/// 켠 노드에만 맞는 배치(`RunningHardware.host_placement`). 마이그레이션·HA는 대상에서 새로 고른다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HostPlacement {
    /// 이 배치를 고른 노드. 지금 노드와 다르면 낡은 기록이다.
    pub node: Uuid,
    /// vCPU 자리(핫플러그 상한까지)마다 pCPU. 공유 VM은 비고 vCPU가 게스트 노드의 호스트 노드 공유 CPU에서 돈다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vcpus: Vec<u16>,
    /// 스레드 정책 `isolate`가 비워 둔 형제 스레드(이 VM 몫으로 센다)
    #[serde(default, skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub siblings: CpuSet,
    /// 에뮬레이터 스레드의 pCPU(`isolate`). 비면 노드의 공유 CPU(그때의 집합)다.
    #[serde(default, skip_serializing_if = "CpuSet::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub emulator: CpuSet,
    /// 게스트 노드마다 호스트 NUMA 노드 id. 비면 묶지 않는다.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub host_nodes: Vec<u16>,
}

// ---------------------------------------------------------------------------------------------------------------
// 검증
// ---------------------------------------------------------------------------------------------------------------

/// 스펙의 NUMA·CPU 정책·hugepages가 맞지 않음(API 400 `invalid_numa`)
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NumaError {
    #[error("cpu.thread_policy needs cpu.policy dedicated")]
    ThreadPolicyNeedsDedicated,
    #[error("cpu.emulator needs cpu.policy dedicated")]
    EmulatorNeedsDedicated,
    #[error(
        "cpu.thread_policy require pairs vCPUs on sibling threads: vCPU slots per socket ({0}) must be even"
    )]
    RequireOdd(u32),
    #[error("1G hugepages cannot be used with memory hotplug (max_memory_mib)")]
    HugepagesHotplug,
    #[error("numa.nodes must be 1 to {MAX_NUMA_NODES}, not {0}")]
    NodeCount(usize),
    #[error(
        "numa.nodes {nodes} must divide cpu.sockets {sockets}: a guest NUMA node holds whole sockets"
    )]
    Sockets { nodes: u16, sockets: u16 },
    #[error(
        "numa.nodes[{index}].cpus {cpus} must be a positive multiple of the vCPU slots per socket ({per_socket})"
    )]
    ListCpus {
        index: usize,
        cpus: u16,
        per_socket: u32,
    },
    #[error("numa.nodes cpus add up to {sum}, not the vCPU limit {max}")]
    ListCpusSum { sum: u32, max: u32 },
    #[error("numa.nodes memory_mib adds up to {sum}, not memory_mib {memory}")]
    ListMemorySum { sum: u64, memory: u32 },
    #[error(
        "memory_mib {memory} cannot be split into {nodes} guest NUMA node(s) of whole {unit} MiB units"
    )]
    MemoryUnit {
        memory: u32,
        nodes: usize,
        unit: u32,
    },
}

/// 이 VM이 계획 64 칸을 하나라도 쓴다(기능 게이트용).
pub fn in_use(spec: &VmSpec) -> bool {
    !spec.cpu.policy.is_shared()
        || spec.cpu.thread_policy.is_some()
        || spec.cpu.emulator.is_some()
        || spec.numa.is_some()
        || spec.hugepages.is_some()
}

/// 스펙의 NUMA 칸을 본다(`VmSpec::validate`가 CPU·메모리 검사 뒤에 부른다).
pub fn validate(spec: &VmSpec) -> Result<(), NumaError> {
    let cpu = &spec.cpu;
    if cpu.policy.is_shared() {
        if cpu.thread_policy.is_some() {
            return Err(NumaError::ThreadPolicyNeedsDedicated);
        }
        if cpu.emulator.is_some() {
            return Err(NumaError::EmulatorNeedsDedicated);
        }
    }
    let per_socket = slots_per_socket(cpu);
    if thread_policy(cpu) == Some(ThreadPolicy::Require) && !per_socket.is_multiple_of(2) {
        return Err(NumaError::RequireOdd(per_socket));
    }
    if spec.hugepages == Some(PageSize::Size1G)
        && spec.max_memory_mib.is_some_and(|m| m > spec.memory_mib)
    {
        return Err(NumaError::HugepagesHotplug);
    }
    let Some(nodes) = wanted_nodes(spec) else {
        return Ok(());
    };
    let unit = memory_unit(spec.hugepages);
    match nodes {
        NumaNodes::Auto => {
            if !spec.memory_mib.is_multiple_of(unit) {
                return Err(NumaError::MemoryUnit {
                    memory: spec.memory_mib,
                    nodes: 1,
                    unit,
                });
            }
        }
        NumaNodes::Count(n) => {
            if n == 0 || n > MAX_NUMA_NODES {
                return Err(NumaError::NodeCount(usize::from(n)));
            }
            if !cpu.sockets.is_multiple_of(n) {
                return Err(NumaError::Sockets {
                    nodes: n,
                    sockets: cpu.sockets,
                });
            }
            if even_split(spec.memory_mib, usize::from(n), unit).is_none() {
                return Err(NumaError::MemoryUnit {
                    memory: spec.memory_mib,
                    nodes: usize::from(n),
                    unit,
                });
            }
        }
        NumaNodes::List(list) => {
            if list.is_empty() || list.len() > usize::from(MAX_NUMA_NODES) {
                return Err(NumaError::NodeCount(list.len()));
            }
            for (index, n) in list.iter().enumerate() {
                if n.cpus == 0 || !u32::from(n.cpus).is_multiple_of(per_socket) {
                    return Err(NumaError::ListCpus {
                        index,
                        cpus: n.cpus,
                        per_socket,
                    });
                }
                if n.memory_mib == 0 || !n.memory_mib.is_multiple_of(unit) {
                    return Err(NumaError::MemoryUnit {
                        memory: spec.memory_mib,
                        nodes: list.len(),
                        unit,
                    });
                }
            }
            let sum: u32 = list.iter().map(|n| u32::from(n.cpus)).sum();
            if sum != cpu.max() {
                return Err(NumaError::ListCpusSum {
                    sum,
                    max: cpu.max(),
                });
            }
            let mem: u64 = list.iter().map(|n| u64::from(n.memory_mib)).sum();
            if mem != u64::from(spec.memory_mib) {
                return Err(NumaError::ListMemorySum {
                    sum: mem,
                    memory: spec.memory_mib,
                });
            }
        }
    }
    Ok(())
}

/// 소켓마다 vCPU 자리 수(핫플러그 상한 / 소켓)
fn slots_per_socket(cpu: &CpuSpec) -> u32 {
    cpu.max() / u32::from(cpu.sockets.max(1))
}

/// 전용 VM의 스레드 정책(기본을 채움). 공유면 None.
fn thread_policy(cpu: &CpuSpec) -> Option<ThreadPolicy> {
    (!cpu.policy.is_shared()).then(|| cpu.thread_policy.unwrap_or_default())
}

/// 전용 VM의 에뮬레이터 정책(기본을 채움). 공유면 None.
fn emulator_policy(cpu: &CpuSpec) -> Option<EmulatorPolicy> {
    (!cpu.policy.is_shared()).then(|| cpu.emulator.unwrap_or_default())
}

/// 실제로 쓰는 메모리 묶기: 준 것, 아니면 전용·hugepages VM은 `strict`
fn memory_policy(spec: &VmSpec) -> Option<MemoryPolicy> {
    spec.numa
        .as_ref()
        .and_then(|n| n.memory)
        .or((!spec.cpu.policy.is_shared() || spec.hugepages.is_some())
            .then_some(MemoryPolicy::Strict))
}

/// 원하는 게스트 노드 나누기. `numa`가 없으면 전용·hugepages VM은 `auto`, 아니면 None(vNUMA 없음).
fn wanted_nodes(spec: &VmSpec) -> Option<NumaNodes> {
    match &spec.numa {
        Some(n) => Some(n.nodes.clone()),
        None if !spec.cpu.policy.is_shared() || spec.hugepages.is_some() => Some(NumaNodes::Auto),
        None => None,
    }
}

/// 게스트 노드 메모리 단위: 1G VM은 1024, 그 밖은 2 MiB(`any`는 2M으로 떨어질 수 있으므로 2)
fn memory_unit(page: Option<PageSize>) -> u32 {
    match page {
        Some(PageSize::Size1G) => PageKind::Size1G.size_mib(),
        _ => SMALL_UNIT_MIB,
    }
}

/// `total`을 `n`개로 고르게 나눈다: 단위 `unit`으로 내리고 남는 것은 마지막 노드에. 노드마다 단위의 배수(0 아님)가
/// 아니면 None.
fn even_split(total: u32, n: usize, unit: u32) -> Option<Vec<u32>> {
    let n32 = u32::try_from(n).ok().filter(|&n| n > 0)?;
    let base = total / n32 / unit * unit;
    if base == 0 {
        return None;
    }
    let last = total - base * (n32 - 1);
    if !last.is_multiple_of(unit) {
        return None;
    }
    let mut out = vec![base; n];
    out[n - 1] = last;
    Some(out)
}

// ---------------------------------------------------------------------------------------------------------------
// 게스트 모양
// ---------------------------------------------------------------------------------------------------------------

/// 스펙에서 뽑은 게스트 모양의 후보(§8). [`fit`]이 앞부터 시도한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// 시도할 나눔(앞부터). 노드 `auto`의 묶는 VM만 둘 이상이다(소켓 수의 약수, 오름차순).
    pub splits: Vec<Vec<SplitNode>>,
    pub page: Option<PageSize>,
    pub policy: CpuPolicy,
    pub thread_policy: Option<ThreadPolicy>,
    pub emulator: Option<EmulatorPolicy>,
    /// 묶는 방법. 있으면 호스트 노드에 맞춰야 한다(전용·hugepages·`numa.memory`).
    pub memory: Option<MemoryPolicy>,
    pub slots_per_socket: u32,
    pub memory_mib: u32,
    /// 고정된 모양([`fit_fixed`])의 게스트 노드 거리
    distances: Option<Vec<Vec<u8>>>,
}

/// 나눔의 게스트 노드 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitNode {
    pub sockets: u16,
    /// 직접 준 메모리. 없으면 고르게 나눈다.
    pub memory_mib: Option<u32>,
}

impl Layout {
    /// 호스트 노드에 묶는 VM이다.
    pub fn binds(&self) -> bool {
        self.memory.is_some()
    }
}

/// 게스트 모양(§8). `numa`가 없는 공유 VM(hugepages도 없음)은 None — 지금 그대로다. `auto`는 소켓 수의 약수 d
/// (d ≤ 호스트 노드 수, d ≤ 8) 가운데 묶는 VM은 맞는 것을 [`fit`]이 고르게 모두 두고, 묶지 않는 VM은 노드마다 vCPU
/// 자리와 메모리가 가장 작은 호스트 노드에 드는 첫 d(없으면 가장 큰 d)를 고른다. 토폴로지가 없으면 d = 1이다.
pub fn layout(spec: &VmSpec, topology: Option<&NodeTopology>) -> Result<Option<Layout>, NumaError> {
    validate(spec)?;
    let Some(nodes) = wanted_nodes(spec) else {
        return Ok(None);
    };
    let per = slots_per_socket(&spec.cpu);
    let sockets = spec.cpu.sockets.max(1);
    let memory = memory_policy(spec);
    let even = |n: u16| {
        vec![
            SplitNode {
                sockets: sockets / n,
                memory_mib: None,
            };
            usize::from(n)
        ]
    };
    let splits = match nodes {
        NumaNodes::Count(n) => vec![even(n)],
        NumaNodes::List(list) => vec![
            list.iter()
                .map(|n| SplitNode {
                    sockets: u16::try_from(u32::from(n.cpus) / per).unwrap_or(u16::MAX),
                    memory_mib: Some(n.memory_mib),
                })
                .collect(),
        ],
        NumaNodes::Auto => {
            let host_nodes = topology.map_or(1, |t| t.nodes.len().max(1));
            let ds: Vec<u16> = (1..=sockets.min(MAX_NUMA_NODES))
                .filter(|&d| sockets.is_multiple_of(d) && usize::from(d) <= host_nodes)
                .collect();
            if memory.is_some() {
                ds.into_iter().map(even).collect()
            } else {
                let fits = |d: u16| {
                    let Some(t) = topology else { return true };
                    let slots = u64::from(sockets / d) * u64::from(per);
                    let mem = u64::from(spec.memory_mib / u32::from(d));
                    t.nodes
                        .iter()
                        .all(|h| h.cpus.len() as u64 >= slots && h.small_memory_mib() >= mem)
                };
                let d = ds
                    .iter()
                    .copied()
                    .find(|&d| fits(d))
                    .or(ds.last().copied())
                    .unwrap_or(1);
                vec![even(d)]
            }
        }
    };
    Ok(Some(Layout {
        splits,
        page: spec.hugepages,
        policy: spec.cpu.policy,
        thread_policy: thread_policy(&spec.cpu),
        emulator: emulator_policy(&spec.cpu),
        memory,
        slots_per_socket: per,
        memory_mib: spec.memory_mib,
        distances: None,
    }))
}

/// 나눔을 게스트 노드로: 소켓은 앞부터 차례로, 메모리는 직접 준 것이나 단위 `unit`의 고른 나눔
fn guest_nodes(split: &[SplitNode], total: u32, unit: u32) -> Option<Vec<GuestNode>> {
    let even = if split.iter().all(|n| n.memory_mib.is_some()) {
        vec![0; split.len()]
    } else {
        even_split(total, split.len(), unit)?
    };
    let mut socket = 0u16;
    let mut out = Vec::with_capacity(split.len());
    for (n, even) in split.iter().zip(even) {
        let memory_mib = n.memory_mib.unwrap_or(even);
        if memory_mib == 0 || !memory_mib.is_multiple_of(unit) {
            return None;
        }
        out.push(GuestNode {
            sockets: (socket..socket + n.sockets).collect(),
            memory_mib,
        });
        socket += n.sockets;
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------------------------
// 호스트 자원
// ---------------------------------------------------------------------------------------------------------------

/// 노드의 세 CPU 집합(§7). 설정이 없으면 예약·전용이 비고 공유 = 온라인 CPU 전체다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSets {
    pub reserved: CpuSet,
    pub dedicated: CpuSet,
    pub shared: CpuSet,
}

impl HostSets {
    /// 설정을 온라인 CPU로 자른다(오프라인 CPU는 어느 집합에도 없다).
    pub fn new(resources: Option<&HostResources>, topology: &NodeTopology) -> HostSets {
        let online = &topology.cpus;
        let Some(r) = resources else {
            return HostSets {
                shared: online.clone(),
                ..HostSets::default()
            };
        };
        let reserved = r.reserved_cpus.intersection(online);
        let dedicated = r.dedicated_cpus.intersection(online).difference(&reserved);
        let shared = online.difference(&reserved).difference(&dedicated);
        HostSets {
            reserved,
            dedicated,
            shared,
        }
    }
}

/// 한 노드에서 도는 VM들이 잡은 것(기록에서 셈, [`usage`])
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostUsage {
    /// 잡힌 전용 pCPU(vCPU·비워 둔 형제·`isolate` 에뮬레이터)
    pub dedicated: CpuSet,
    /// (호스트 노드 id, 크기) → 페이지 수
    pub pages: BTreeMap<(u16, PageKind), u64>,
}

impl HostUsage {
    /// 둘을 더한다.
    pub fn add(&mut self, other: &HostUsage) {
        self.dedicated = self.dedicated.union(&other.dedicated);
        for (k, v) in &other.pages {
            *self.pages.entry(*k).or_default() += v;
        }
    }
}

/// 기록(`hardware.numa`·`host_placement`) 가운데 노드 `node`의 것을 더한다. 다른 노드의 배치는 낡은 기록이라 건너뛴다.
pub fn usage<'a>(
    records: impl IntoIterator<Item = (&'a ResolvedNuma, &'a HostPlacement)>,
    node: Uuid,
) -> HostUsage {
    let mut out = HostUsage::default();
    for (numa, place) in records {
        if place.node != node {
            continue;
        }
        out.dedicated.0.extend(place.vcpus.iter().copied());
        out.dedicated = out.dedicated.union(&place.siblings).union(&place.emulator);
        if let Some(page) = numa.page {
            for (g, h) in numa.nodes.iter().zip(&place.host_nodes) {
                *out.pages.entry((*h, page)).or_default() += pages_for(g.memory_mib, page);
            }
        }
    }
    out
}

/// 메모리 `mib`에 드는 크기 `page` 페이지 수
fn pages_for(mib: u32, page: PageKind) -> u64 {
    u64::from(mib).div_ceil(u64::from(page.size_mib()))
}

/// 노드의 빈 자원: 전용 집합에서 잡힌 것을 빼고, 풀 전체에서 쓴 페이지를 뺀다. 노드 할당기는 여기에 `/sys`의 실제 빈
/// 수를 더 견준다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostFree {
    pub dedicated: CpuSet,
    /// (호스트 노드 id, 크기) → 빈 페이지 수
    pub pages: BTreeMap<(u16, PageKind), u64>,
}

impl HostFree {
    pub fn new(topology: &NodeTopology, sets: &HostSets, used: &HostUsage) -> HostFree {
        let mut pages = BTreeMap::new();
        for n in &topology.nodes {
            for kind in [PageKind::Size2M, PageKind::Size1G] {
                let total = n.pages(kind);
                if total > 0 {
                    let taken = used.pages.get(&(n.id, kind)).copied().unwrap_or(0);
                    pages.insert((n.id, kind), total.saturating_sub(taken));
                }
            }
        }
        HostFree {
            dedicated: sets.dedicated.difference(&used.dedicated),
            pages,
        }
    }

    fn pages(&self, node: u16, kind: PageKind) -> u64 {
        self.pages.get(&(node, kind)).copied().unwrap_or(0)
    }
}

/// [`fit`]이 보는 노드 하나
#[derive(Debug, Clone, Copy)]
pub struct HostView<'a> {
    pub node: Uuid,
    pub topology: &'a NodeTopology,
    pub sets: &'a HostSets,
    pub free: &'a HostFree,
}

/// 맞춘 결과. 묶지 않는 VM은 `placement`가 없다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fit {
    pub numa: ResolvedNuma,
    pub placement: Option<HostPlacement>,
}

/// 노드에 맞지 않는 까닭 하나. 노드 이름은 부르는 쪽이 [`NumaRefusal::for_node`]로 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NumaRefusal {
    #[error("cannot pin vCPUs or bind memory (no topology)")]
    NoTopology,
    #[error("cannot pin vCPUs")]
    NoPinning,
    #[error("has no dedicated CPUs")]
    NoDedicatedCpus,
    #[error("has no SMT sibling threads for cpu.thread_policy require")]
    NoSmt,
    #[error("has no {}", pages_name(*.0))]
    NoPages(Option<PageKind>),
    #[error("has {have} host NUMA node(s); the VM needs {need}")]
    HostNodes { have: usize, need: usize },
    #[error("has {} free dedicated CPUs on host NUMA {}; the VM needs {need} on {}",
        list_values(free), list_nodes(free), on_each(*guests))]
    DedicatedCpus {
        /// (호스트 노드 id, 빈 전용 CPU 수)
        free: Vec<(u16, u64)>,
        need: u64,
        guests: usize,
    },
    #[error("has {} free {kind} hugepages on host NUMA {}; the VM needs {need} on {}",
        list_values(free), list_nodes(free), on_each(*guests))]
    Pages {
        kind: PageKind,
        free: Vec<(u16, u64)>,
        need: u64,
        guests: usize,
    },
    #[error("has no shared CPUs on {} host NUMA node(s) for the VM", .0)]
    SharedCpus(usize),
    #[error("has {} MiB of memory on host NUMA {}; the VM needs {need} MiB on {}",
        list_values(free), list_nodes(free), on_each(*guests))]
    Memory {
        free: Vec<(u16, u64)>,
        need: u64,
        guests: usize,
    },
    #[error("cannot split {memory} MiB into {nodes} guest NUMA node(s) of whole {unit} MiB units")]
    Split {
        memory: u32,
        nodes: usize,
        unit: u32,
    },
    #[error("cannot place the VM's {0} guest NUMA nodes on distinct host NUMA nodes")]
    Combined(usize),
}

impl NumaRefusal {
    /// `node "n1" has 2 and 3 free dedicated CPUs on host NUMA nodes 0 and 1; the VM needs 4 on one`
    pub fn for_node(&self, name: &str) -> String {
        format!("node {name:?} {self}")
    }
}

fn pages_name(kind: Option<PageKind>) -> String {
    match kind {
        Some(k) => format!("{k} hugepages"),
        None => "hugepages".into(),
    }
}

fn and_list(items: Vec<String>) -> String {
    match items.len() {
        0 => String::new(),
        1 => items[0].clone(),
        n => format!("{} and {}", items[..n - 1].join(", "), items[n - 1]),
    }
}

fn list_values(v: &[(u16, u64)]) -> String {
    and_list(v.iter().map(|(_, n)| n.to_string()).collect())
}

fn list_nodes(v: &[(u16, u64)]) -> String {
    let ids = and_list(v.iter().map(|(id, _)| id.to_string()).collect());
    if v.len() == 1 {
        format!("node {ids}")
    } else {
        format!("nodes {ids}")
    }
}

fn on_each(guests: usize) -> String {
    if guests == 1 {
        "one".into()
    } else {
        format!("each of {guests}")
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 맞춤
// ---------------------------------------------------------------------------------------------------------------

/// 게스트 모양을 노드에 맞춘다(§8). 묶지 않는 VM은 노드와 상관없이 첫 나눔이다. 묶는 VM은 나눔(앞부터) × 페이지 크기
/// (`any`는 1G 먼저)마다 게스트 노드를 서로 다른 호스트 노드에 놓아 보고 처음 되는 것을 돌려준다. 모두 안 되면 첫
/// 나눔의 까닭이다.
pub fn fit(layout: &Layout, host: Option<&HostView<'_>>) -> Result<Fit, NumaRefusal> {
    if !layout.binds() {
        let split = &layout.splits[0];
        let nodes =
            guest_nodes(split, layout.memory_mib, SMALL_UNIT_MIB).ok_or(NumaRefusal::Split {
                memory: layout.memory_mib,
                nodes: split.len(),
                unit: SMALL_UNIT_MIB,
            })?;
        return Ok(Fit {
            numa: ResolvedNuma {
                nodes,
                distances: layout.distances.clone().unwrap_or_default(),
                page: None,
                policy: layout.policy,
                thread_policy: layout.thread_policy,
                emulator: layout.emulator,
                memory: None,
            },
            placement: None,
        });
    }
    let pages: Vec<Option<PageKind>> = match layout.page {
        None => vec![None],
        Some(p) => p.kinds().into_iter().map(Some).collect(),
    };
    let mut first = None;
    for split in &layout.splits {
        let mut errors = Vec::new();
        for &page in &pages {
            match fit_one(layout, split, page, host) {
                Ok(fit) => return Ok(fit),
                Err(e) => errors.push(e),
            }
        }
        if first.is_none() {
            // `any`가 두 크기 모두 풀이 없으면 크기 없이 말한다.
            first = if errors.len() > 1
                && errors.iter().all(|e| matches!(e, NumaRefusal::NoPages(_)))
            {
                Some(NumaRefusal::NoPages(None))
            } else {
                errors.pop()
            };
        }
    }
    Err(first.unwrap_or(NumaRefusal::Combined(0)))
}

/// 이미 켠 VM(마이그레이션 대상·메모리 스냅샷 불러오기): 게스트 모양·페이지·거리는 기록 그대로 두고 호스트 노드와
/// pCPU만 고른다. `cpu`는 켤 때의 CPU(`hw.cpu`, 소켓당 자리 수)다.
pub fn fit_fixed(
    numa: &ResolvedNuma,
    cpu: &CpuSpec,
    host: Option<&HostView<'_>>,
) -> Result<Fit, NumaRefusal> {
    let layout = Layout {
        splits: vec![
            numa.nodes
                .iter()
                .map(|n| SplitNode {
                    sockets: u16::try_from(n.sockets.len()).unwrap_or(u16::MAX),
                    memory_mib: Some(n.memory_mib),
                })
                .collect(),
        ],
        page: numa.page.map(PageSize::from),
        policy: numa.policy,
        thread_policy: numa.thread_policy,
        emulator: numa.emulator,
        memory: numa.memory,
        slots_per_socket: slots_per_socket(cpu),
        memory_mib: numa.nodes.iter().map(|n| n.memory_mib).sum(),
        distances: Some(numa.distances.clone()),
    };
    let mut fit = fit(&layout, host)?;
    // 소켓 번호까지 기록 그대로(나눔은 앞부터 차례이므로 같지만, 기록을 믿는다).
    fit.numa.nodes = numa.nodes.clone();
    Ok(fit)
}

/// 게스트 노드 하나를 호스트 노드 하나에 놓을 때
#[derive(Debug, Clone, Default)]
struct Cell {
    cpus: Option<Pick>,
    shared: bool,
    pages: bool,
    memory: bool,
    /// 맞춘 뒤 남는 전용 CPU 수와 페이지 MiB(채우기 순서)
    left: (u64, u64),
}

impl Cell {
    fn ok(&self, dedicated: bool) -> bool {
        (if dedicated {
            self.cpus.is_some()
        } else {
            self.shared
        }) && self.pages
            && self.memory
    }
}

/// 고른 pCPU
#[derive(Debug, Clone, Default)]
struct Pick {
    vcpus: Vec<u16>,
    siblings: Vec<u16>,
    emulator: Option<u16>,
}

fn fit_one(
    layout: &Layout,
    split: &[SplitNode],
    page: Option<PageKind>,
    host: Option<&HostView<'_>>,
) -> Result<Fit, NumaRefusal> {
    let host = host.ok_or(NumaRefusal::NoTopology)?;
    let topo = host.topology;
    let dedicated = !layout.policy.is_shared();
    if dedicated && !topo.pinning {
        return Err(NumaRefusal::NoPinning);
    }
    if dedicated && host.sets.dedicated.is_empty() {
        return Err(NumaRefusal::NoDedicatedCpus);
    }
    let thread = layout.thread_policy.unwrap_or_default();
    if dedicated && thread == ThreadPolicy::Require && topo.threads_per_core < 2 {
        return Err(NumaRefusal::NoSmt);
    }
    if let Some(k) = page
        && topo.pages(k) == 0
    {
        return Err(NumaRefusal::NoPages(Some(k)));
    }
    let unit = page.map_or(SMALL_UNIT_MIB, PageKind::size_mib);
    let guests = guest_nodes(split, layout.memory_mib, unit).ok_or(NumaRefusal::Split {
        memory: layout.memory_mib,
        nodes: split.len(),
        unit,
    })?;
    let k = guests.len();
    if topo.nodes.len() < k {
        return Err(NumaRefusal::HostNodes {
            have: topo.nodes.len(),
            need: k,
        });
    }
    let isolate_emulator = dedicated && layout.emulator == Some(EmulatorPolicy::Isolate);
    let slots: Vec<u32> = guests
        .iter()
        .map(|g| g.sockets.len() as u32 * layout.slots_per_socket)
        .collect();
    let need_pages: Vec<u64> = guests
        .iter()
        .map(|g| page.map_or(0, |p| pages_for(g.memory_mib, p)))
        .collect();
    // cells[i][h]
    let mut cells = vec![vec![Cell::default(); topo.nodes.len()]; k];
    for (i, g) in guests.iter().enumerate() {
        for (h, hn) in topo.nodes.iter().enumerate() {
            let cell = &mut cells[i][h];
            let free_ded = host.free.dedicated.intersection(&hn.cpus);
            if dedicated {
                cell.cpus = pick_cpus(
                    topo,
                    &hn.cpus,
                    &free_ded,
                    slots[i],
                    thread,
                    isolate_emulator && i == 0,
                );
            }
            cell.shared = !host.sets.shared.is_disjoint(&hn.cpus);
            let used = cell.cpus.as_ref().map_or(0, |p| {
                p.vcpus.len() + p.siblings.len() + usize::from(p.emulator.is_some())
            });
            let mut left_pages = 0;
            cell.pages = match page {
                None => true,
                Some(p) => {
                    let free = host.free.pages(hn.id, p);
                    left_pages = free.saturating_sub(need_pages[i]) * u64::from(p.size_mib());
                    free >= need_pages[i]
                }
            };
            cell.memory = page.is_some() || hn.small_memory_mib() >= u64::from(g.memory_mib);
            cell.left = (
                (free_ded.len() - used.min(free_ded.len())) as u64,
                left_pages,
            );
        }
    }
    let key = |a: &[usize]| -> (u8, u64, u64, Vec<u16>) {
        let mut far = 0;
        for (x, &p) in a.iter().enumerate() {
            for &q in &a[x + 1..] {
                far = far.max(topo.distance(p, q));
            }
        }
        let (mut cpus, mut pages) = (0, 0);
        for (i, &h) in a.iter().enumerate() {
            cpus += cells[i][h].left.0;
            pages += cells[i][h].left.1;
        }
        (
            far,
            cpus,
            pages,
            a.iter().map(|&h| topo.nodes[h].id).collect(),
        )
    };
    let hosts = topo.nodes.len();
    let Some(chosen) = assign(k, hosts, &|i, h| cells[i][h].ok(dedicated), &key) else {
        let per_node = |f: &dyn Fn(&HostNode) -> u64| -> Vec<(u16, u64)> {
            topo.nodes.iter().map(|n| (n.id, f(n))).collect()
        };
        if dedicated && assign(k, hosts, &|i, h| cells[i][h].cpus.is_some(), &key).is_none() {
            let extra = u64::from(isolate_emulator);
            let factor = if thread == ThreadPolicy::Isolate {
                u64::from(topo.threads_per_core.max(1))
            } else {
                1
            };
            let need = slots
                .iter()
                .map(|&s| u64::from(s) * factor)
                .max()
                .unwrap_or(0)
                + extra;
            return Err(NumaRefusal::DedicatedCpus {
                free: per_node(&|n| host.free.dedicated.intersection(&n.cpus).len() as u64),
                need,
                guests: k,
            });
        }
        if let Some(p) = page
            && assign(k, hosts, &|i, h| cells[i][h].pages, &key).is_none()
        {
            return Err(NumaRefusal::Pages {
                kind: p,
                free: per_node(&|n| host.free.pages(n.id, p)),
                need: need_pages.iter().copied().max().unwrap_or(0),
                guests: k,
            });
        }
        if !dedicated && assign(k, hosts, &|i, h| cells[i][h].shared, &key).is_none() {
            return Err(NumaRefusal::SharedCpus(k));
        }
        if assign(k, hosts, &|i, h| cells[i][h].memory, &key).is_none() {
            return Err(NumaRefusal::Memory {
                free: per_node(&HostNode::small_memory_mib),
                need: guests
                    .iter()
                    .map(|g| u64::from(g.memory_mib))
                    .max()
                    .unwrap_or(0),
                guests: k,
            });
        }
        return Err(NumaRefusal::Combined(k));
    };
    let mut placement = HostPlacement {
        node: host.node,
        vcpus: Vec::new(),
        siblings: CpuSet::new(),
        emulator: CpuSet::new(),
        host_nodes: chosen.iter().map(|&h| topo.nodes[h].id).collect(),
    };
    for (i, &h) in chosen.iter().enumerate() {
        if let Some(p) = &cells[i][h].cpus {
            placement.vcpus.extend(&p.vcpus);
            placement.siblings.0.extend(p.siblings.iter().copied());
            placement.emulator.0.extend(p.emulator);
        }
    }
    let distances = match &layout.distances {
        Some(d) => d.clone(),
        None if k >= 2 && !topo.distances.is_empty() => chosen
            .iter()
            .map(|&a| chosen.iter().map(|&b| topo.distance(a, b)).collect())
            .collect(),
        None => Vec::new(),
    };
    Ok(Fit {
        numa: ResolvedNuma {
            nodes: guests,
            distances,
            page,
            policy: layout.policy,
            thread_policy: layout.thread_policy,
            emulator: layout.emulator,
            memory: layout.memory,
        },
        placement: Some(placement),
    })
}

type Key = (u8, u64, u64, Vec<u16>);

/// 게스트 노드 `0..k`를 서로 다른 호스트 노드 `0..hosts`에 놓는다(`ok(i, h)`). 순열이 적으면 모두 보고 `key`가 가장
/// 작은 것을, 많으면 게스트 노드마다 욕심쟁이로 고른다.
fn assign(
    k: usize,
    hosts: usize,
    ok: &dyn Fn(usize, usize) -> bool,
    key: &dyn Fn(&[usize]) -> Key,
) -> Option<Vec<usize>> {
    let perms = (0..k).fold(1u64, |acc, j| {
        acc.saturating_mul(hosts.saturating_sub(j) as u64)
    });
    if perms <= MAX_PERMUTATIONS {
        let mut best: Option<(Key, Vec<usize>)> = None;
        let mut cur = Vec::with_capacity(k);
        search(k, hosts, ok, key, &mut cur, &mut best);
        return best.map(|(_, a)| a);
    }
    let mut cur: Vec<usize> = Vec::with_capacity(k);
    for i in 0..k {
        let h = (0..hosts)
            .filter(|h| !cur.contains(h) && ok(i, *h))
            .min_by_key(|&h| {
                let mut a = cur.clone();
                a.push(h);
                key(&a)
            })?;
        cur.push(h);
    }
    Some(cur)
}

fn search(
    k: usize,
    hosts: usize,
    ok: &dyn Fn(usize, usize) -> bool,
    key: &dyn Fn(&[usize]) -> Key,
    cur: &mut Vec<usize>,
    best: &mut Option<(Key, Vec<usize>)>,
) {
    let i = cur.len();
    if i == k {
        let key = key(cur);
        if best.as_ref().is_none_or(|(b, _)| key < *b) {
            *best = Some((key, cur.clone()));
        }
        return;
    }
    for h in 0..hosts {
        if !cur.contains(&h) && ok(i, h) {
            cur.push(h);
            search(k, hosts, ok, key, cur, best);
            cur.pop();
        }
    }
}

/// 고르는 단위: (vCPU에 줄 pCPU들, 함께 잡는 형제)
type Unit = (Vec<u16>, Vec<u16>);

/// 호스트 노드 하나(`node_cpus`) 안에서 빈 전용 pCPU(`avail`)로 vCPU `slots`개(+ 에뮬레이터 하나)를 고른다. L3 묶음
/// 하나에 다 들면 번호가 낮은 묶음, 아니면 큰 묶음부터 적게 쓴다. 묶음 안에서는 번호가 낮은 코어부터다.
fn pick_cpus(
    topo: &NodeTopology,
    node_cpus: &CpuSet,
    avail: &CpuSet,
    slots: u32,
    thread: ThreadPolicy,
    emulator: bool,
) -> Option<Pick> {
    // 코어: 형제 묶음 ∩ 이 노드, 묶음에 없는 CPU는 혼자 코어
    let mut cores: Vec<Vec<u16>> = topo
        .siblings
        .iter()
        .map(|s| s.intersection(node_cpus).iter().collect::<Vec<_>>())
        .filter(|c| !c.is_empty())
        .collect();
    let in_group: CpuSet = cores.iter().flatten().copied().collect();
    cores.extend(node_cpus.difference(&in_group).iter().map(|c| vec![c]));
    cores.sort();
    let l3_of = |cpu: u16| {
        topo.l3
            .iter()
            .position(|g| g.contains(cpu))
            .unwrap_or(usize::MAX)
    };
    let mut groups: BTreeMap<usize, Vec<Unit>> = BTreeMap::new();
    for core in &cores {
        let free: Vec<u16> = core
            .iter()
            .copied()
            .filter(|&c| avail.contains(c))
            .collect();
        let units: Vec<Unit> = match thread {
            ThreadPolicy::Prefer => free.iter().map(|&c| (vec![c], Vec::new())).collect(),
            ThreadPolicy::Isolate if free.len() == core.len() => {
                vec![(vec![core[0]], core[1..].to_vec())]
            }
            ThreadPolicy::Isolate => Vec::new(),
            ThreadPolicy::Require if free.len() >= 2 => vec![(free[..2].to_vec(), Vec::new())],
            ThreadPolicy::Require => Vec::new(),
        };
        for u in units {
            groups.entry(l3_of(u.0[0])).or_default().push(u);
        }
    }
    let cap = |units: &[Unit]| units.iter().map(|u| u.0.len() as u32).sum::<u32>();
    let need = slots;
    // 고른 묶음(큰 것부터 채운다)
    let chosen: Vec<usize> = match groups.iter().find(|(_, u)| cap(u) >= need) {
        Some((&g, _)) => vec![g],
        None => {
            let mut by_size: Vec<(usize, u32)> = groups.iter().map(|(&g, u)| (g, cap(u))).collect();
            by_size.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let mut sum = 0;
            let mut out = Vec::new();
            for (g, c) in by_size {
                if sum >= need {
                    break;
                }
                sum += c;
                out.push(g);
            }
            if sum < need {
                return None;
            }
            out
        }
    };
    let mut pick = Pick::default();
    for g in &chosen {
        for (v, s) in &groups[g] {
            if pick.vcpus.len() as u32 >= need {
                break;
            }
            pick.vcpus.extend(v);
            pick.siblings.extend(s);
        }
    }
    if pick.vcpus.len() as u32 != need {
        return None;
    }
    if emulator {
        let taken: CpuSet = pick.vcpus.iter().chain(&pick.siblings).copied().collect();
        let rest = avail.difference(&taken);
        let near = rest.iter().find(|&c| chosen.contains(&l3_of(c)));
        pick.emulator = Some(near.or(rest.first())?);
    }
    Some(pick)
}

// ---------------------------------------------------------------------------------------------------------------
// 자원 요구와 보류
// ---------------------------------------------------------------------------------------------------------------

/// VM이 노드에서 잡는 자원의 합(§8). Raft 받아들이기 검사가 이것만 본다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Demand {
    /// 전용 pCPU 수(자리 × 스레드 정책의 몫 + 에뮬레이터 `isolate` 1)
    pub pcpus: u64,
    /// 크기마다 hugepages 메모리(MiB)
    pub pages: BTreeMap<PageKind, u64>,
    /// `any`의 hugepages 메모리(MiB, 두 크기 합에 센다)
    pub any_mib: u64,
}

impl Demand {
    pub fn is_zero(&self) -> bool {
        self.pcpus == 0 && self.pages.is_empty() && self.any_mib == 0
    }
}

/// VM 스펙의 요구. `isolate`는 코어의 모든 스레드를 잡으므로 노드의 `threads_per_core`를 곱한다(토폴로지가 없으면 1).
pub fn demand(spec: &VmSpec, topology: Option<&NodeTopology>) -> Demand {
    let mut d = Demand::default();
    if !spec.cpu.policy.is_shared() {
        let factor = match thread_policy(&spec.cpu) {
            Some(ThreadPolicy::Isolate) => {
                topology.map_or(1, |t| u64::from(t.threads_per_core.max(1)))
            }
            _ => 1,
        };
        d.pcpus = u64::from(spec.cpu.max()) * factor
            + u64::from(emulator_policy(&spec.cpu) == Some(EmulatorPolicy::Isolate));
    }
    let mib = u64::from(spec.memory_mib);
    match spec.hugepages {
        None => {}
        Some(PageSize::Size2M) => {
            d.pages.insert(PageKind::Size2M, mib);
        }
        Some(PageSize::Size1G) => {
            d.pages.insert(PageKind::Size1G, mib);
        }
        Some(PageSize::Any) => d.any_mib = mib,
    }
    d
}

impl Demand {
    /// 둘을 더한다.
    pub fn add(&mut self, other: &Demand) {
        self.pcpus += other.pcpus;
        for (k, v) in &other.pages {
            *self.pages.entry(*k).or_default() += v;
        }
        self.any_mib += other.any_mib;
    }

    /// 어느 자원도 `other`보다 많지 않다(이 쓰기는 장부를 늘리지 않는다).
    pub fn within(&self, other: &Demand) -> bool {
        self.pcpus <= other.pcpus
            && self.any_mib <= other.any_mib
            && self.pages.iter().all(|(k, v)| *v <= other.mib(*k))
    }

    /// 크기 `kind`의 hugepages 메모리(MiB, `any` 빼고)
    pub fn mib(&self, kind: PageKind) -> u64 {
        self.pages.get(&kind).copied().unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 노드 자원 장부(§9)
// ---------------------------------------------------------------------------------------------------------------

/// 장부가 세는 노드 자원(Raft 거절 `HostResourcesExceeded`의 `resource`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum HostResource {
    /// 전용 pCPU 수
    #[serde(rename = "dedicated_cpus")]
    DedicatedCpus,
    /// 2M hugepages 메모리(MiB)
    #[serde(rename = "hugepages_2m")]
    Hugepages2M,
    /// 1G hugepages 메모리(MiB)
    #[serde(rename = "hugepages_1g")]
    Hugepages1G,
    /// 두 크기 합의 hugepages 메모리(MiB) — `any` VM이 여기에 센다
    #[serde(rename = "hugepages")]
    Hugepages,
}

impl HostResource {
    pub fn as_str(self) -> &'static str {
        match self {
            HostResource::DedicatedCpus => "dedicated_cpus",
            HostResource::Hugepages2M => "hugepages_2m",
            HostResource::Hugepages1G => "hugepages_1g",
            HostResource::Hugepages => "hugepages",
        }
    }

    /// 문구의 단위
    pub fn unit(self) -> &'static str {
        match self {
            HostResource::DedicatedCpus => "dedicated CPUs",
            HostResource::Hugepages2M => "MiB of 2M hugepages",
            HostResource::Hugepages1G => "MiB of 1G hugepages",
            HostResource::Hugepages => "MiB of hugepages",
        }
    }
}

/// 노드가 내줄 수 있는 것: 전용 집합(온라인)의 크기와 크기마다 풀 전체(MiB). 노드 설정과 노드 상태에서 셈하므로
/// 복제본마다 같다. 토폴로지가 없으면 모두 0이다(그 노드는 고정·hugepages를 못 한다).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostLimits {
    pub pcpus: u64,
    pub mib_2m: u64,
    pub mib_1g: u64,
}

impl HostLimits {
    pub fn new(resources: Option<&HostResources>, topology: Option<&NodeTopology>) -> HostLimits {
        let Some(t) = topology else {
            return HostLimits::default();
        };
        let pool = |k: PageKind| t.pages(k) * u64::from(k.size_mib());
        HostLimits {
            pcpus: HostSets::new(resources, t).dedicated.len() as u64,
            mib_2m: pool(PageKind::Size2M),
            mib_1g: pool(PageKind::Size1G),
        }
    }
}

/// 상한을 넘는 자원 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("would use {used} of {limit} {}", resource.unit())]
pub struct HostExceeded {
    pub resource: HostResource,
    pub limit: u64,
    pub used: u64,
}

/// 노드 하나의 장부 판정(§9): 쓰기 뒤 합 `after`가 상한을 넘고 쓰기 전 합 `before`보다 는 첫 자원(전용 CPU → 2M →
/// 1G → 두 크기 합). 상한이 줄어 이미 넘친 노드에서도 늘리지 않는 쓰기는 된다. Raft 상태 머신과 스케줄러가 같은 것을
/// 쓴다.
pub fn exceeded(limits: &HostLimits, before: &Demand, after: &Demand) -> Option<HostExceeded> {
    let hugepages = |d: &Demand| d.mib(PageKind::Size2M) + d.mib(PageKind::Size1G) + d.any_mib;
    [
        (
            HostResource::DedicatedCpus,
            limits.pcpus,
            before.pcpus,
            after.pcpus,
        ),
        (
            HostResource::Hugepages2M,
            limits.mib_2m,
            before.mib(PageKind::Size2M),
            after.mib(PageKind::Size2M),
        ),
        (
            HostResource::Hugepages1G,
            limits.mib_1g,
            before.mib(PageKind::Size1G),
            after.mib(PageKind::Size1G),
        ),
        (
            HostResource::Hugepages,
            limits.mib_2m + limits.mib_1g,
            hugepages(before),
            hugepages(after),
        ),
    ]
    .into_iter()
    .find(|(_, limit, was, now)| now > limit && now > was)
    .map(|(resource, limit, _, used)| HostExceeded {
        resource,
        limit,
        used,
    })
}

/// 기록의 짧은 이름: `2 nodes, dedicated (isolate threads, isolated emulator), hugepages 1G, memory strict`
pub fn describe(numa: Option<&ResolvedNuma>) -> String {
    let Some(n) = numa else {
        return "none".into();
    };
    let mut out = vec![nodes_name(n.nodes.len())];
    out.extend(describe_policy(n.policy, n.thread_policy, n.emulator));
    if let Some(p) = n.page {
        out.push(format!("hugepages {p}"));
    }
    if let Some(m) = n.memory {
        out.push(format!("memory {}", m.as_str()));
    }
    out.join(", ")
}

/// 스펙의 짧은 이름(보류의 `wanted`)
pub fn describe_spec(spec: &VmSpec) -> String {
    let nodes = match wanted_nodes(spec) {
        None => return "none".into(),
        Some(NumaNodes::Auto) => "auto".to_string(),
        Some(NumaNodes::Count(n)) => nodes_name(usize::from(n)),
        Some(NumaNodes::List(l)) => nodes_name(l.len()),
    };
    let mut out = vec![nodes];
    out.extend(describe_policy(
        spec.cpu.policy,
        thread_policy(&spec.cpu),
        emulator_policy(&spec.cpu),
    ));
    if let Some(p) = spec.hugepages {
        out.push(format!("hugepages {}", p.as_str()));
    }
    if let Some(m) = memory_policy(spec) {
        out.push(format!("memory {}", m.as_str()));
    }
    out.join(", ")
}

fn nodes_name(n: usize) -> String {
    if n == 1 {
        "1 node".into()
    } else {
        format!("{n} nodes")
    }
}

fn describe_policy(
    policy: CpuPolicy,
    thread: Option<ThreadPolicy>,
    emulator: Option<EmulatorPolicy>,
) -> Option<String> {
    if policy.is_shared() {
        return None;
    }
    let mut extra = Vec::new();
    if let Some(t) = thread.filter(|t| *t != ThreadPolicy::Prefer) {
        extra.push(format!("{} threads", t.as_str()));
    }
    if emulator == Some(EmulatorPolicy::Isolate) {
        extra.push("isolated emulator".to_string());
    }
    Some(if extra.is_empty() {
        "dedicated".into()
    } else {
        format!("dedicated ({})", extra.join(", "))
    })
}

/// 실행 중 VM의 보류 `numa`(순수, §10). 스펙의 CPU 정책·스레드·에뮬레이터·vNUMA·hugepages·메모리 묶기가 켤 때의
/// 기록(`hw.numa`, 없으면 vNUMA 없는 공유 VM)과 다르면 그것이다. `auto`는 기록의 어느 모양이든 받고, `any`는 기록의
/// 어느 크기든 받는다. 이유는 늘 `Restart`(전원 껐다 켜기)이고 설명은 바뀐 것과 [`POWER_CYCLE`]이다.
pub fn pending_change(spec: &VmSpec, hw: &RunningHardware) -> Option<PendingChange> {
    let run = hw.numa.as_ref();
    let mut what = Vec::new();
    let run_policy = run.map_or(CpuPolicy::Shared, |n| n.policy);
    if spec.cpu.policy != run_policy {
        what.push(format!(
            "{} → {}",
            run_policy.as_str(),
            spec.cpu.policy.as_str()
        ));
    }
    let name = |t: Option<&'static str>| t.unwrap_or("none");
    let (want_t, run_t) = (thread_policy(&spec.cpu), run.and_then(|n| n.thread_policy));
    if !spec.cpu.policy.is_shared() && !run_policy.is_shared() && want_t != run_t {
        what.push(format!(
            "threads {} → {}",
            name(run_t.map(ThreadPolicy::as_str)),
            name(want_t.map(ThreadPolicy::as_str))
        ));
    }
    let (want_e, run_e) = (emulator_policy(&spec.cpu), run.and_then(|n| n.emulator));
    if !spec.cpu.policy.is_shared() && !run_policy.is_shared() && want_e != run_e {
        what.push(format!(
            "emulator threads {} → {}",
            name(run_e.map(EmulatorPolicy::as_str)),
            name(want_e.map(EmulatorPolicy::as_str))
        ));
    }
    let run_page = run.and_then(|n| n.page);
    let page_ok = match (spec.hugepages, run_page) {
        (None, None) => true,
        (Some(want), Some(p)) => want.accepts(p),
        _ => false,
    };
    if !page_ok {
        what.push(format!(
            "hugepages {} → {}",
            name(run_page.map(PageKind::as_str)),
            name(spec.hugepages.map(PageSize::as_str))
        ));
    }
    let (want_m, run_m) = (memory_policy(spec), run.and_then(|n| n.memory));
    if want_m != run_m {
        what.push(format!(
            "memory {} → {}",
            name(run_m.map(MemoryPolicy::as_str)),
            name(want_m.map(MemoryPolicy::as_str))
        ));
    }
    let per = slots_per_socket(&spec.cpu);
    let nodes_ok = match (wanted_nodes(spec), run) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(NumaNodes::Auto), Some(_)) => true,
        (Some(NumaNodes::Count(n)), Some(r)) => usize::from(n) == r.nodes.len(),
        (Some(NumaNodes::List(l)), Some(r)) => {
            l.len() == r.nodes.len()
                && l.iter().zip(&r.nodes).all(|(w, g)| {
                    u32::from(w.cpus) == g.sockets.len() as u32 * per
                        && w.memory_mib == g.memory_mib
                })
        }
    };
    if !nodes_ok {
        let want = match wanted_nodes(spec) {
            None => "none".to_string(),
            Some(NumaNodes::Auto) => "auto".to_string(),
            Some(NumaNodes::Count(n)) => nodes_name(usize::from(n)),
            Some(NumaNodes::List(l)) => format!("{} (list)", nodes_name(l.len())),
        };
        let have = run.map_or("none".to_string(), |r| nodes_name(r.nodes.len()));
        what.push(format!("NUMA {have} → {want}"));
    }
    if what.is_empty() {
        return None;
    }
    Some(PendingChange {
        field: "numa".into(),
        running: describe(run),
        wanted: describe_spec(spec),
        reason: PendingReason::Restart,
        detail: Some(format!("{}; {POWER_CYCLE}", what.join(", "))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{NodeSpec, NodeStatus};
    use serde_json::json;

    const NODE: Uuid = Uuid::from_u128(7);

    fn set(s: &str) -> CpuSet {
        s.parse().unwrap()
    }

    /// `json` 문자열을 `T`로 풀어 다시 쓰면 바이트까지 같다.
    fn same_bytes<T: Serialize + for<'de> Deserialize<'de>>(text: &str) {
        let v: T = serde_json::from_str(text).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), text);
    }

    fn vm(extra: serde_json::Value) -> VmSpec {
        let mut j = json!({
            "cpu": { "sockets": 2, "cores": 4 },
            "memory_mib": 8192,
            "machine": "q35",
        });
        for (k, v) in extra.as_object().unwrap() {
            if k == "cpu" {
                for (ck, cv) in v.as_object().unwrap() {
                    j["cpu"][ck] = cv.clone();
                }
            } else {
                j[k] = v.clone();
            }
        }
        let spec: VmSpec = serde_json::from_value(j).unwrap();
        spec
    }

    /// 실제 2소켓 Xeon 모양: 소켓 2 × 코어 8 × 스레드 2, 노드 2(노드 0 = CPU 0-7·16-23, 노드 1 = 8-15·24-31), 형제
    /// (i, i+16), L3는 소켓마다, 거리 10·21, 노드마다 64 GiB와 1G 4개·2M 512개
    fn xeon() -> NodeTopology {
        let node = |id: u16, cpus: &str| HostNode {
            id,
            cpus: set(cpus),
            memory_mib: 65536,
            hugepages: vec![
                HugepagePool {
                    size_kib: 2048,
                    total: 512,
                },
                HugepagePool {
                    size_kib: 1048576,
                    total: 4,
                },
            ],
        };
        NodeTopology {
            sockets: 2,
            threads_per_core: 2,
            cpus: set("0-31"),
            nodes: vec![node(0, "0-7,16-23"), node(1, "8-15,24-31")],
            distances: vec![vec![10, 21], vec![21, 10]],
            siblings: (0..16).map(|i| [i, i + 16].into_iter().collect()).collect(),
            l3: vec![set("0-7,16-23"), set("8-15,24-31")],
            page_sizes_kib: vec![2048, 1048576],
            pinning: true,
            cpuset: true,
            ..NodeTopology::default()
        }
    }

    /// SMT 없는 호스트 노드 넷(노드마다 CPU 4개, 16 GiB, 2M 1024개), 거리: 0-1·2-3 가깝고(12) 나머지 멂(30)
    fn quad() -> NodeTopology {
        let near = |a: usize, b: usize| a / 2 == b / 2;
        NodeTopology {
            sockets: 4,
            threads_per_core: 1,
            cpus: set("0-15"),
            nodes: (0..4u16)
                .map(|i| HostNode {
                    id: i,
                    cpus: (i * 4..i * 4 + 4).collect(),
                    memory_mib: 16384,
                    hugepages: vec![HugepagePool {
                        size_kib: 2048,
                        total: 1024,
                    }],
                })
                .collect(),
            distances: (0..4)
                .map(|a| {
                    (0..4)
                        .map(|b| {
                            if a == b {
                                10
                            } else if near(a, b) {
                                12
                            } else {
                                30
                            }
                        })
                        .collect()
                })
                .collect(),
            pinning: true,
            ..NodeTopology::default()
        }
    }

    fn resources(reserved: &str, dedicated: &str) -> HostResources {
        HostResources {
            reserved_cpus: set(reserved),
            dedicated_cpus: set(dedicated),
            hugepages: Vec::new(),
        }
    }

    /// 노드에 맞춘다(쓴 것 `used`를 뺀 빈 자원으로).
    fn fit_on(
        spec: &VmSpec,
        topo: &NodeTopology,
        res: Option<&HostResources>,
        used: &HostUsage,
    ) -> Result<Fit, NumaRefusal> {
        let sets = HostSets::new(res, topo);
        let free = HostFree::new(topo, &sets, used);
        let view = HostView {
            node: NODE,
            topology: topo,
            sets: &sets,
            free: &free,
        };
        let layout = layout(spec, Some(topo)).unwrap().unwrap();
        fit(&layout, Some(&view))
    }

    // ---------------------------------------------------------------------------------------------------------
    // CpuSet
    // ---------------------------------------------------------------------------------------------------------

    #[test]
    fn cpu_set_parses_and_writes_the_canonical_form() {
        for (text, canon) in [
            ("0-3,8,10-11", "0-3,8,10-11"),
            ("8,0,1,2,3,10,11", "0-3,8,10-11"),
            ("3-3", "3"),
            (" 0-1 , 2\n", "0-2"),
            ("", ""),
            ("4095", "4095"),
            ("0,2,4", "0,2,4"),
        ] {
            let s: CpuSet = text.parse().unwrap();
            assert_eq!(s.to_string(), canon, "{text:?}");
            assert_eq!(serde_json::to_value(&s).unwrap(), json!(canon));
            assert_eq!(serde_json::from_value::<CpuSet>(json!(canon)).unwrap(), s);
        }
        for bad in ["4096", "3-1", "a", "1-", "-1", "1,,2", "0-70000"] {
            assert!(bad.parse::<CpuSet>().is_err(), "{bad:?}");
            assert!(serde_json::from_value::<CpuSet>(json!(bad)).is_err());
        }
        let a = set("0-7");
        let b = set("4-11");
        assert_eq!(a.intersection(&b), set("4-7"));
        assert_eq!(a.union(&b), set("0-11"));
        assert_eq!(a.difference(&b), set("0-3"));
        assert!(set("1-2").is_subset(&a) && set("8").is_disjoint(&a));
        assert_eq!((a.len(), a.first()), (8, Some(0)));
    }

    // ---------------------------------------------------------------------------------------------------------
    // serde 하위 호환
    // ---------------------------------------------------------------------------------------------------------

    #[test]
    fn old_json_stays_byte_identical() {
        // 계획 63 끝의 VM·노드·기록 JSON(새 칸 없음)은 풀어 다시 써도 바이트까지 같다.
        same_bytes::<VmSpec>(
            r#"{"cpu":{"sockets":2,"cores":4,"model":"host","max_vcpus":16},"memory_mib":8192,"max_memory_mib":16384,"firmware":"uefi","machine":"q35","disks":[],"nics":[],"power":"running","restart":"on-failure","node":null,"snapshot_schedule":null,"ha":false}"#,
        );
        same_bytes::<CpuSpec>(r#"{"sockets":1,"cores":2,"model":"max","flags":["+aes"]}"#);
        same_bytes::<NodeSpec>(
            r#"{"address":"10.0.0.1:8443","maintenance":false,"witness":false}"#,
        );
        same_bytes::<NodeSpec>(
            r#"{"address":"10.0.0.1:8443","maintenance":true,"witness":false,"roles":["api"]}"#,
        );
        same_bytes::<NodeStatus>(
            r#"{"online":true,"cpus":4,"memory_mib":8192,"hypervisor":"qemu-kvm","bridges":[],"arch":"x86_64","pools":{},"features":["numa"]}"#,
        );
        same_bytes::<RunningHardware>(
            r#"{"cpu":{"sockets":1,"cores":2,"model":"host"},"memory_mib":4096,"firmware":"uefi","machine":"q35"}"#,
        );
        same_bytes::<crate::folder::FolderQuota>(r#"{"vcpus":32,"vms":20}"#);
        same_bytes::<crate::folder::FolderUsage>(
            r#"{"vcpus":8,"memory_mib":4096,"disk_bytes":0,"vms":2}"#,
        );
        // 이 기능을 쓰지 않는 스펙은 새 칸을 쓰지 않는다.
        let v = serde_json::to_value(vm(json!({}))).unwrap();
        for k in ["numa", "hugepages"] {
            assert!(v.get(k).is_none(), "{k}");
        }
        for k in ["policy", "thread_policy", "emulator"] {
            assert!(v["cpu"].get(k).is_none(), "{k}");
        }
    }

    #[test]
    fn new_fields_round_trip() {
        let spec = vm(json!({
            "cpu": { "policy": "dedicated", "thread_policy": "isolate", "emulator": "isolate" },
            "numa": { "nodes": [{ "cpus": 4, "memory_mib": 4096 }, { "cpus": 4, "memory_mib": 4096 }],
                      "memory": "preferred" },
            "hugepages": "1G",
        }));
        assert_eq!(spec.cpu.policy, CpuPolicy::Dedicated);
        assert_eq!(spec.hugepages, Some(PageSize::Size1G));
        let back: VmSpec = serde_json::from_value(serde_json::to_value(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
        for (nodes, want) in [
            (json!("auto"), NumaNodes::Auto),
            (json!(2), NumaNodes::Count(2)),
            (
                json!([{ "cpus": 8, "memory_mib": 16384 }]),
                NumaNodes::List(vec![NumaNodeSpec {
                    cpus: 8,
                    memory_mib: 16384,
                }]),
            ),
        ] {
            let n: NumaSpec = serde_json::from_value(json!({ "nodes": nodes })).unwrap();
            assert_eq!(n.nodes, want);
            assert_eq!(serde_json::to_value(&n).unwrap(), json!({ "nodes": nodes }));
        }
        assert_eq!(
            serde_json::from_value::<NumaSpec>(json!({})).unwrap().nodes,
            NumaNodes::Auto
        );
        for bad in [
            json!({ "nodes": "two" }),
            json!({ "nodes": -1 }),
            json!({ "nodes": 2, "memroy": "strict" }),
            json!({ "nodes": [{ "cpus": 2 }] }),
        ] {
            assert!(
                serde_json::from_value::<NumaSpec>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        for (p, s) in [
            (PageSize::Size2M, "2M"),
            (PageSize::Size1G, "1G"),
            (PageSize::Any, "any"),
        ] {
            assert_eq!(serde_json::to_value(p).unwrap(), json!(s));
        }
        // 노드 쪽
        let node: NodeSpec = serde_json::from_value(json!({
            "address": "a:1",
            "host_resources": { "reserved_cpus": "0-1", "dedicated_cpus": "4-31",
                                "hugepages": [{ "size": "2M", "count": 1024, "host_node": 0 }] },
        }))
        .unwrap();
        let hr = node.host_resources.as_ref().unwrap();
        assert_eq!(hr.dedicated_cpus, set("4-31"));
        assert_eq!(hr.hugepages[0].size, PageKind::Size2M);
        let back: NodeSpec = serde_json::from_value(serde_json::to_value(&node).unwrap()).unwrap();
        assert_eq!(back, node);
        let status = NodeStatus {
            topology: Some(xeon()),
            ..NodeStatus::default()
        };
        let text = serde_json::to_string(&status).unwrap();
        assert!(text.len() < 4096, "{}", text.len());
        assert_eq!(serde_json::from_str::<NodeStatus>(&text).unwrap(), status);
    }

    // ---------------------------------------------------------------------------------------------------------
    // 검증
    // ---------------------------------------------------------------------------------------------------------

    #[test]
    fn validate_rejects_bad_combinations() {
        let err = |extra: serde_json::Value| vm(extra).validate().unwrap_err().to_string();
        assert!(err(json!({ "cpu": { "thread_policy": "isolate" } })).contains("thread_policy"));
        assert!(err(json!({ "cpu": { "emulator": "isolate" } })).contains("emulator"));
        assert!(
            err(
                json!({ "cpu": { "policy": "dedicated", "thread_policy": "require", "cores": 3 } })
            )
            .contains("even")
        );
        assert!(
            err(json!({ "hugepages": "1G", "max_memory_mib": 16384 })).contains("memory hotplug")
        );
        assert!(err(json!({ "numa": { "nodes": 0 } })).contains("1 to 8"));
        assert!(err(json!({ "numa": { "nodes": 9 } })).contains("1 to 8"));
        assert!(err(json!({ "numa": { "nodes": 4 } })).contains("divide"));
        assert!(
            err(json!({ "numa": { "nodes": [{ "cpus": 3, "memory_mib": 4096 }, { "cpus": 5, "memory_mib": 4096 }] } }))
                .contains("multiple")
        );
        assert!(
            err(json!({ "numa": { "nodes": [{ "cpus": 4, "memory_mib": 4096 }] } }))
                .contains("vCPU limit")
        );
        assert!(
            err(json!({ "numa": { "nodes": [{ "cpus": 4, "memory_mib": 4096 }, { "cpus": 4, "memory_mib": 2048 }] } }))
                .contains("adds up")
        );
        // 1G VM은 노드마다 1024의 배수
        assert!(err(json!({ "hugepages": "1G", "memory_mib": 3000 })).contains("1024 MiB"));
        assert!(
            err(json!({ "hugepages": "1G", "memory_mib": 1024, "numa": { "nodes": 2 } }))
                .contains("1024 MiB")
        );
        // 되는 것
        for ok in [
            json!({}),
            json!({ "numa": { "nodes": 2 } }),
            json!({ "numa": { "nodes": "auto", "memory": "strict" } }),
            json!({ "cpu": { "policy": "dedicated", "thread_policy": "require" } }),
            json!({ "hugepages": "1G", "memory_mib": 3072, "numa": { "nodes": 2 } }),
            json!({ "hugepages": "2M", "max_memory_mib": 16384 }),
            json!({ "cpu": { "max_vcpus": 16 },
                    "numa": { "nodes": [{ "cpus": 8, "memory_mib": 6144 }, { "cpus": 8, "memory_mib": 2048 }] } }),
        ] {
            assert_eq!(vm(ok.clone()).validate(), Ok(()), "{ok}");
        }
        assert!(!in_use(&vm(json!({}))));
        assert!(in_use(&vm(json!({ "hugepages": "any" }))));
        assert!(in_use(&vm(json!({ "cpu": { "policy": "dedicated" } }))));
    }

    #[test]
    fn host_resources_validate_against_the_topology() {
        let t = xeon();
        assert_eq!(resources("0-1", "4-15").validate(&t), Ok(()));
        assert!(matches!(
            resources("0-1", "30-40").validate(&t),
            Err(HostResourcesError::NotOnline { .. })
        ));
        assert!(matches!(
            resources("0-4", "4-15").validate(&t),
            Err(HostResourcesError::Overlap(s)) if s == set("4")
        ));
        assert_eq!(
            resources("0-1", "2-31").validate(&t),
            Err(HostResourcesError::NoShared)
        );
        let mut r = resources("", "");
        r.hugepages.push(HugepageReserve {
            size: PageKind::Size2M,
            count: 1,
            host_node: Some(5),
        });
        assert_eq!(r.validate(&t), Err(HostResourcesError::HostNode(5)));
        r.hugepages[0].host_node = Some(1);
        r.hugepages.push(r.hugepages[0]);
        assert_eq!(
            r.validate(&t),
            Err(HostResourcesError::Duplicate(PageKind::Size2M))
        );
        let sets = HostSets::new(Some(&resources("0-1", "4-15,40")), &t);
        assert_eq!(sets.dedicated, set("4-15"));
        assert_eq!(sets.shared, set("2-3,16-31"));
        assert_eq!(HostSets::new(None, &t).shared, set("0-31"));
    }

    // ---------------------------------------------------------------------------------------------------------
    // 게스트 모양
    // ---------------------------------------------------------------------------------------------------------

    fn split_sizes(l: &Layout) -> Vec<usize> {
        l.splits.iter().map(Vec::len).collect()
    }

    #[test]
    fn layout_table() {
        let t = xeon();
        // 쓰지 않으면 없음
        assert_eq!(layout(&vm(json!({})), Some(&t)).unwrap(), None);
        // 개수: 소켓 차례로, 메모리 고르게
        let l = layout(&vm(json!({ "numa": { "nodes": 2 } })), None)
            .unwrap()
            .unwrap();
        assert!(!l.binds());
        let f = fit(&l, None).unwrap();
        assert_eq!(f.placement, None);
        assert_eq!(
            f.numa.nodes,
            vec![
                GuestNode {
                    sockets: vec![0],
                    memory_mib: 4096
                },
                GuestNode {
                    sockets: vec![1],
                    memory_mib: 4096
                }
            ]
        );
        assert!(f.numa.distances.is_empty() && f.numa.page.is_none());
        // 고르지 않은 메모리: 단위로 내리고 남는 것은 마지막에
        let l = layout(
            &vm(json!({ "cpu": { "sockets": 4, "cores": 1 }, "memory_mib": 3000, "numa": { "nodes": 4 } })),
            None,
        )
        .unwrap()
        .unwrap();
        let f = fit(&l, None).unwrap();
        assert_eq!(
            f.numa
                .nodes
                .iter()
                .map(|n| n.memory_mib)
                .collect::<Vec<_>>(),
            vec![750, 750, 750, 750]
        );
        let l = layout(
            &vm(json!({ "cpu": { "sockets": 4, "cores": 1 }, "memory_mib": 3002, "numa": { "nodes": 4 } })),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            fit(&l, None)
                .unwrap()
                .numa
                .nodes
                .iter()
                .map(|n| n.memory_mib)
                .collect::<Vec<_>>(),
            vec![750, 750, 750, 752]
        );
        // 직접 목록과 핫플러그 자리: 상한 16 = 소켓 2 × 자리 8, 노드 0은 소켓 0, 노드 1은 소켓 1
        let l = layout(
            &vm(json!({ "cpu": { "max_vcpus": 16 },
                        "numa": { "nodes": [{ "cpus": 8, "memory_mib": 6144 }, { "cpus": 8, "memory_mib": 2048 }] } })),
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(l.slots_per_socket, 8);
        let f = fit(&l, None).unwrap();
        assert_eq!(
            f.numa
                .nodes
                .iter()
                .map(|n| (n.sockets.clone(), n.memory_mib))
                .collect::<Vec<_>>(),
            vec![(vec![0], 6144), (vec![1], 2048)]
        );
        // auto, 묶지 않음: 호스트 노드보다 클 때만 나눈다.
        let auto = |cores: u16, memory: u32| {
            let l = layout(
                &vm(json!({ "cpu": { "cores": cores }, "memory_mib": memory, "numa": { "nodes": "auto" } })),
                Some(&t),
            )
            .unwrap()
            .unwrap();
            split_sizes(&l)
        };
        assert_eq!(auto(4, 8192), vec![1]); // 8 자리, 8 GiB: 노드 하나에 든다
        assert_eq!(auto(12, 8192), vec![2]); // 24 자리 > 노드의 16 CPU
        assert_eq!(auto(4, 100_000), vec![2]); // 메모리 > 노드의 64 GiB
        assert_eq!(auto(64, 8192), vec![2]); // 둘로도 안 들어도 가장 큰 d
        // 토폴로지가 없거나 소켓이 하나면 1
        assert_eq!(
            split_sizes(
                &layout(
                    &vm(json!({ "cpu": { "cores": 12 }, "numa": { "nodes": "auto" } })),
                    None
                )
                .unwrap()
                .unwrap()
            ),
            vec![1]
        );
        assert_eq!(
            split_sizes(
                &layout(
                    &vm(
                        json!({ "cpu": { "sockets": 1, "cores": 24 }, "numa": { "nodes": "auto" } })
                    ),
                    Some(&t)
                )
                .unwrap()
                .unwrap()
            ),
            vec![1]
        );
        // 묶는 VM의 auto는 후보를 모두 둔다(호스트 노드 수까지).
        let l = layout(
            &vm(json!({ "cpu": { "sockets": 4, "cores": 2 }, "hugepages": "2M" })),
            Some(&t),
        )
        .unwrap()
        .unwrap();
        assert!(l.binds());
        assert_eq!(l.memory, Some(MemoryPolicy::Strict));
        assert_eq!(split_sizes(&l), vec![1, 2]);
        let l = layout(
            &vm(json!({ "cpu": { "sockets": 4, "cores": 2 }, "hugepages": "2M" })),
            Some(&quad()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(split_sizes(&l), vec![1, 2, 4]);
        // 주면 공유 VM도 묶는다.
        let l = layout(
            &vm(json!({ "numa": { "nodes": 2, "memory": "preferred" } })),
            Some(&t),
        )
        .unwrap()
        .unwrap();
        assert_eq!(l.memory, Some(MemoryPolicy::Preferred));
    }

    // ---------------------------------------------------------------------------------------------------------
    // 맞춤
    // ---------------------------------------------------------------------------------------------------------

    #[test]
    fn fit_dedicated_on_one_host_node_prefers_siblings_and_low_numbers() {
        let t = xeon();
        let res = resources("0-1,16-17", "2-15,18-31");
        let spec = vm(json!({ "cpu": { "sockets": 1, "cores": 4, "policy": "dedicated" } }));
        let f = fit_on(&spec, &t, Some(&res), &HostUsage::default()).unwrap();
        let p = f.placement.unwrap();
        // prefer: 코어 2(2·18), 코어 3(3·19) — 형제를 먼저 채운다.
        assert_eq!(p.vcpus, vec![2, 18, 3, 19]);
        assert_eq!(p.host_nodes, vec![0]);
        assert!(p.emulator.is_empty() && p.siblings.is_empty());
        assert_eq!(f.numa.memory, Some(MemoryPolicy::Strict));
        assert_eq!(f.numa.thread_policy, Some(ThreadPolicy::Prefer));
        assert_eq!(f.numa.emulator, Some(EmulatorPolicy::Shared));
        assert!(f.numa.distances.is_empty());
        assert_eq!(p.node, NODE);
    }

    #[test]
    fn fit_thread_policies_with_and_without_smt() {
        let t = xeon();
        let res = resources("0-1,16-17", "2-15,18-31");
        let none = HostUsage::default();
        let spec = |thread: &str| {
            vm(
                json!({ "cpu": { "sockets": 1, "cores": 4, "policy": "dedicated", "thread_policy": thread } }),
            )
        };
        // isolate: 코어마다 하나, 형제는 비워 둔다.
        let p = fit_on(&spec("isolate"), &t, Some(&res), &none)
            .unwrap()
            .placement
            .unwrap();
        assert_eq!(p.vcpus, vec![2, 3, 4, 5]);
        assert_eq!(p.siblings, set("18-21"));
        // require: 형제 짝
        let p = fit_on(&spec("require"), &t, Some(&res), &none)
            .unwrap()
            .placement
            .unwrap();
        assert_eq!(p.vcpus, vec![2, 18, 3, 19]);
        // isolate는 형제 하나가 잡힌 코어를 쓰지 않는다.
        let used = HostUsage {
            dedicated: set("18-20"),
            ..HostUsage::default()
        };
        let p = fit_on(&spec("isolate"), &t, Some(&res), &used)
            .unwrap()
            .placement
            .unwrap();
        // 노드 0에는 통째로 빈 코어가 5·6·7 셋뿐이라 노드 1로
        assert_eq!((p.vcpus, p.host_nodes), (vec![8, 9, 10, 11], vec![1]));
        // SMT 없는 호스트
        let q = quad();
        let qres = resources("0", "1-15");
        let s = vm(
            json!({ "cpu": { "sockets": 1, "cores": 2, "policy": "dedicated", "thread_policy": "require" } }),
        );
        assert_eq!(fit_on(&s, &q, Some(&qres), &none), Err(NumaRefusal::NoSmt));
        let s = vm(
            json!({ "cpu": { "sockets": 1, "cores": 2, "policy": "dedicated", "thread_policy": "isolate" } }),
        );
        assert_eq!(
            fit_on(&s, &q, Some(&qres), &none)
                .unwrap()
                .placement
                .unwrap()
                .vcpus,
            vec![1, 2]
        );
    }

    #[test]
    fn fit_emulator_isolate_takes_one_more_on_guest_node_zero() {
        let t = xeon();
        let res = resources("0-1,16-17", "2-15,18-31");
        let spec = vm(
            json!({ "cpu": { "sockets": 2, "cores": 2, "policy": "dedicated", "emulator": "isolate" },
                              "numa": { "nodes": 2 } }),
        );
        let f = fit_on(&spec, &t, Some(&res), &HostUsage::default()).unwrap();
        let p = f.placement.unwrap();
        assert_eq!(p.host_nodes, vec![0, 1]);
        assert_eq!(p.vcpus, vec![2, 18, 8, 24]);
        assert_eq!(p.emulator, set("3"));
        assert_eq!(f.numa.distances, vec![vec![10, 21], vec![21, 10]]);
        let d = demand(&spec, Some(&t));
        assert_eq!(d.pcpus, 5);
        // 기록에서 센 사용
        let used = usage([(&f.numa, &p)], NODE);
        assert_eq!(used.dedicated, set("2-3,8,18,24"));
        assert!(
            usage([(&f.numa, &p)], Uuid::from_u128(9))
                .dedicated
                .is_empty()
        );
    }

    #[test]
    fn fit_refuses_with_one_reason() {
        let t = xeon();
        let none = HostUsage::default();
        let ded = |cores: u16| {
            vm(json!({ "cpu": { "sockets": 1, "cores": cores, "policy": "dedicated" } }))
        };
        // 토폴로지·고정·전용 집합
        let l = layout(&ded(2), None).unwrap().unwrap();
        assert_eq!(fit(&l, None), Err(NumaRefusal::NoTopology));
        let mut nopin = t.clone();
        nopin.pinning = false;
        assert_eq!(
            fit_on(&ded(2), &nopin, Some(&resources("", "4-7")), &none),
            Err(NumaRefusal::NoPinning)
        );
        assert_eq!(
            fit_on(&ded(2), &t, None, &none),
            Err(NumaRefusal::NoDedicatedCpus)
        );
        // 노드마다 2·3개 비어 4개를 못 놓는다.
        let res = resources("", "4-5,8-10");
        let e = fit_on(&ded(4), &t, Some(&res), &none).unwrap_err();
        assert_eq!(
            e.for_node("n1"),
            "node \"n1\" has 2 and 3 free dedicated CPUs on host NUMA nodes 0 and 1; the VM needs 4 on one"
        );
        // hugepages
        let mut no1g = t.clone();
        for n in &mut no1g.nodes {
            n.hugepages.retain(|p| p.size_kib == 2048);
        }
        let s = vm(json!({ "hugepages": "1G" }));
        assert_eq!(
            fit_on(&s, &no1g, None, &none).unwrap_err().for_node("n2"),
            "node \"n2\" has no 1G hugepages"
        );
        let s = vm(json!({ "memory_mib": 8192, "hugepages": "1G", "cpu": { "sockets": 1 } }));
        assert_eq!(
            fit_on(&s, &t, None, &none).unwrap_err().to_string(),
            "has 4 and 4 free 1G hugepages on host NUMA nodes 0 and 1; the VM needs 8 on one"
        );
        let mut bare = t.clone();
        for n in &mut bare.nodes {
            n.hugepages.clear();
        }
        assert_eq!(
            fit_on(&vm(json!({ "hugepages": "any" })), &bare, None, &none),
            Err(NumaRefusal::NoPages(None))
        );
        assert_eq!(NumaRefusal::NoPages(None).to_string(), "has no hugepages");
        // 게스트 노드가 호스트 노드보다 많음
        assert_eq!(
            fit_on(
                &vm(
                    json!({ "cpu": { "sockets": 4, "cores": 1 }, "numa": { "nodes": 4, "memory": "strict" } })
                ),
                &t,
                None,
                &none
            ),
            Err(NumaRefusal::HostNodes { have: 2, need: 4 })
        );
        // 묶는 공유 VM의 메모리
        assert_eq!(
            fit_on(
                &vm(json!({ "cpu": { "sockets": 1 }, "memory_mib": 70000, "numa": { "nodes": 1, "memory": "strict" } })),
                &t,
                None,
                &none
            )
            .unwrap_err()
            .to_string(),
            "has 60416 and 60416 MiB of memory on host NUMA nodes 0 and 1; the VM needs 70000 MiB on one"
        );
    }

    #[test]
    fn fit_hugepages_2m_1g_and_any() {
        let t = xeon();
        let none = HostUsage::default();
        // 1G: 4 GiB VM는 노드 하나의 1G 4개
        let s = vm(json!({ "cpu": { "sockets": 1 }, "memory_mib": 4096, "hugepages": "1G" }));
        let f = fit_on(&s, &t, None, &none).unwrap();
        assert_eq!(f.numa.page, Some(PageKind::Size1G));
        let p = f.placement.unwrap();
        assert_eq!(p.host_nodes, vec![0]);
        assert!(p.vcpus.is_empty());
        let used = usage([(&f.numa, &p)], NODE);
        assert_eq!(used.pages[&(0, PageKind::Size1G)], 4);
        // 노드 0의 1G가 다 쓰였으면 노드 1
        let f2 = fit_on(&s, &t, None, &used).unwrap();
        assert_eq!(f2.placement.unwrap().host_nodes, vec![1]);
        // any: 1G가 없으면 2M
        let s = vm(json!({ "cpu": { "sockets": 1 }, "memory_mib": 1024, "hugepages": "any" }));
        let mut used = HostUsage::default();
        used.pages.insert((0, PageKind::Size1G), 4);
        used.pages.insert((1, PageKind::Size1G), 4);
        let f = fit_on(&s, &t, None, &used).unwrap();
        assert_eq!(f.numa.page, Some(PageKind::Size2M));
        let u = usage([(&f.numa, f.placement.as_ref().unwrap())], NODE);
        assert_eq!(u.pages[&(0, PageKind::Size2M)], 512);
        // any에 1G가 있으면 1G
        assert_eq!(
            fit_on(&s, &t, None, &none).unwrap().numa.page,
            Some(PageKind::Size1G)
        );
        // 2M, 두 게스트 노드
        let s = vm(json!({ "memory_mib": 2048, "hugepages": "2M", "numa": { "nodes": 2 } }));
        let f = fit_on(&s, &t, None, &none).unwrap();
        assert_eq!(f.placement.unwrap().host_nodes, vec![0, 1]);
        assert_eq!(f.numa.nodes[0].memory_mib, 1024);
    }

    #[test]
    fn fit_auto_splits_only_when_one_host_node_is_too_small() {
        let t = xeon();
        let res = resources("", "0-31");
        let none = HostUsage::default();
        // 소켓 2 × 코어 4 = 8 전용: 노드 하나(16 전용)에 든다 → d = 1
        let s = vm(json!({ "cpu": { "policy": "dedicated" } }));
        let f = fit_on(&s, &t, Some(&res), &none).unwrap();
        assert_eq!(f.numa.nodes.len(), 1);
        assert_eq!(f.numa.nodes[0].sockets, vec![0, 1]);
        // 소켓 2 × 코어 12 = 24 전용: 노드 하나에 안 든다 → d = 2
        let s = vm(json!({ "cpu": { "cores": 12, "policy": "dedicated" } }));
        let f = fit_on(&s, &t, Some(&res), &none).unwrap();
        assert_eq!(f.numa.nodes.len(), 2);
        assert_eq!(f.placement.unwrap().vcpus.len(), 24);
        // 소켓 하나면 나눌 수 없다.
        let s = vm(json!({ "cpu": { "sockets": 1, "cores": 24, "policy": "dedicated" } }));
        assert!(matches!(
            fit_on(&s, &t, Some(&res), &none),
            Err(NumaRefusal::DedicatedCpus { need: 24, .. })
        ));
    }

    #[test]
    fn fit_orders_by_distance_then_packing_then_id() {
        let q = quad();
        let res = resources("", "0-15");
        let spec = vm(
            json!({ "cpu": { "sockets": 2, "cores": 2, "policy": "dedicated" }, "numa": { "nodes": 2 } }),
        );
        // 비어 있으면 가까운 짝 중 번호가 낮은 0·1
        let f = fit_on(&spec, &q, Some(&res), &HostUsage::default()).unwrap();
        assert_eq!(f.placement.as_ref().unwrap().host_nodes, vec![0, 1]);
        assert_eq!(f.numa.distances, vec![vec![10, 12], vec![12, 10]]);
        // 노드 1이 둘 잡혀 있으면: 가까운 짝 2·3이 0·1(남는 것 적음)보다 … 0·1이 남는 것이 적고 거리도 같다.
        let used = HostUsage {
            dedicated: set("4-5"),
            ..HostUsage::default()
        };
        let f = fit_on(&spec, &q, Some(&res), &used).unwrap();
        assert_eq!(f.placement.as_ref().unwrap().host_nodes, vec![0, 1]);
        assert_eq!(f.placement.unwrap().vcpus, vec![0, 1, 6, 7]);
        // 노드 1이 셋 잡혀 있으면 0·1은 안 되고 2·3(가까움)이 0·2(멂)보다 앞선다.
        let used = HostUsage {
            dedicated: set("4-6"),
            ..HostUsage::default()
        };
        let f = fit_on(&spec, &q, Some(&res), &used).unwrap();
        assert_eq!(f.placement.unwrap().host_nodes, vec![2, 3]);
        // 채우기: 노드 하나짜리 VM은 남는 것이 적은 노드(1, 둘 남음)로
        let one = vm(json!({ "cpu": { "sockets": 1, "cores": 2, "policy": "dedicated" } }));
        let used = HostUsage {
            dedicated: set("4-5"),
            ..HostUsage::default()
        };
        let f = fit_on(&one, &q, Some(&res), &used).unwrap();
        assert_eq!(f.placement.unwrap().vcpus, vec![6, 7]);
    }

    #[test]
    fn fit_picks_the_fewest_l3_groups() {
        // 노드 하나, CPU 0-7, L3 둘(0-3, 4-7). 0·1이 잡혀 있으면 3개짜리 VM은 4-6(L3 하나)
        let t = NodeTopology {
            sockets: 1,
            threads_per_core: 1,
            cpus: set("0-7"),
            nodes: vec![HostNode {
                id: 0,
                cpus: set("0-7"),
                memory_mib: 16384,
                hugepages: Vec::new(),
            }],
            l3: vec![set("0-3"), set("4-7")],
            pinning: true,
            ..NodeTopology::default()
        };
        let res = resources("", "0-7");
        let used = HostUsage {
            dedicated: set("0-1"),
            ..HostUsage::default()
        };
        let s = vm(json!({ "cpu": { "sockets": 1, "cores": 3, "policy": "dedicated" } }));
        assert_eq!(
            fit_on(&s, &t, Some(&res), &used)
                .unwrap()
                .placement
                .unwrap()
                .vcpus,
            vec![4, 5, 6]
        );
        // 둘이면 낮은 L3(2·3)
        let s = vm(json!({ "cpu": { "sockets": 1, "cores": 2, "policy": "dedicated" } }));
        assert_eq!(
            fit_on(&s, &t, Some(&res), &used)
                .unwrap()
                .placement
                .unwrap()
                .vcpus,
            vec![2, 3]
        );
        // 다섯이면 큰 묶음(4-7)을 먼저 채우고 나머지(2)
        let s = vm(json!({ "cpu": { "sockets": 1, "cores": 5, "policy": "dedicated" } }));
        assert_eq!(
            fit_on(&s, &t, Some(&res), &used)
                .unwrap()
                .placement
                .unwrap()
                .vcpus,
            vec![4, 5, 6, 7, 2]
        );
    }

    #[test]
    fn fit_shared_bound_vm_takes_no_cpus() {
        let t = xeon();
        let s = vm(json!({ "numa": { "nodes": 2, "memory": "preferred" } }));
        let f = fit_on(
            &s,
            &t,
            Some(&resources("0-1", "2-15")),
            &HostUsage::default(),
        )
        .unwrap();
        let p = f.placement.unwrap();
        assert_eq!(p.host_nodes, vec![0, 1]);
        assert!(p.vcpus.is_empty() && p.emulator.is_empty());
        assert_eq!(f.numa.memory, Some(MemoryPolicy::Preferred));
        assert_eq!(f.numa.policy, CpuPolicy::Shared);
        assert_eq!(f.numa.thread_policy, None);
        // 호스트 노드 1의 CPU가 모두 전용이면 공유 vCPU가 갈 곳이 없다.
        let res = resources("", "8-15,24-31");
        assert_eq!(
            fit_on(&s, &t, Some(&res), &HostUsage::default()),
            Err(NumaRefusal::SharedCpus(2))
        );
    }

    #[test]
    fn fit_fixed_keeps_the_recorded_shape() {
        let t = xeon();
        let res = resources("", "2-15,18-31");
        let spec = vm(
            json!({ "cpu": { "policy": "dedicated" }, "memory_mib": 2048, "numa": { "nodes": 2 }, "hugepages": "2M" }),
        );
        let f = fit_on(&spec, &t, Some(&res), &HostUsage::default()).unwrap();
        // 다른 노드에서 다시: 노드 0의 전용이 다 잡혀 있어도 모양은 그대로, 호스트 쪽만 새로
        let sets = HostSets::new(Some(&res), &t);
        let used = HostUsage {
            dedicated: set("2-7"),
            ..HostUsage::default()
        };
        let free = HostFree::new(&t, &sets, &used);
        let view = HostView {
            node: Uuid::from_u128(8),
            topology: &t,
            sets: &sets,
            free: &free,
        };
        let again = fit_fixed(&f.numa, &spec.cpu, Some(&view)).unwrap();
        assert_eq!(again.numa, f.numa);
        let p = again.placement.unwrap();
        assert_eq!(p.node, Uuid::from_u128(8));
        assert_eq!(p.vcpus.len(), 8);
        assert!(p.vcpus.iter().all(|c| !used.dedicated.contains(*c)));
        // 공유 vNUMA 기록도 그대로
        let s = vm(json!({ "numa": { "nodes": 2 } }));
        let rec = fit(&layout(&s, None).unwrap().unwrap(), None).unwrap().numa;
        assert_eq!(fit_fixed(&rec, &s.cpu, None).unwrap().numa, rec);
    }

    #[test]
    fn demand_counts_slots_threads_emulator_and_pages() {
        let t = xeon();
        assert!(demand(&vm(json!({})), Some(&t)).is_zero());
        let d = demand(
            &vm(json!({ "cpu": { "policy": "dedicated", "max_vcpus": 16 } })),
            Some(&t),
        );
        assert_eq!(d.pcpus, 16);
        let d = demand(
            &vm(json!({ "cpu": { "policy": "dedicated", "thread_policy": "isolate" } })),
            Some(&t),
        );
        assert_eq!(d.pcpus, 16);
        assert_eq!(
            demand(
                &vm(json!({ "cpu": { "policy": "dedicated", "thread_policy": "isolate" } })),
                None
            )
            .pcpus,
            8
        );
        let d = demand(&vm(json!({ "hugepages": "1G" })), Some(&t));
        assert_eq!(d.pages, BTreeMap::from([(PageKind::Size1G, 8192)]));
        let d = demand(&vm(json!({ "hugepages": "any" })), Some(&t));
        assert_eq!((d.pcpus, d.any_mib), (0, 8192));
    }

    #[test]
    fn host_limits_count_the_dedicated_set_and_pools() {
        let t = xeon();
        assert_eq!(HostLimits::new(None, None), HostLimits::default());
        let l = HostLimits::new(Some(&resources("0-1", "1-9")), Some(&t));
        // 예약과 겹친 1은 전용이 아니다.
        assert_eq!(l.pcpus, 8);
        assert_eq!(l.mib_2m, 2 * 512 * 2);
        assert_eq!(l.mib_1g, 2 * 4 * 1024);
        assert_eq!(HostLimits::new(None, Some(&t)).pcpus, 0);
    }

    #[test]
    fn exceeded_only_when_over_and_grown() {
        let limits = HostLimits {
            pcpus: 4,
            mib_2m: 2048,
            mib_1g: 4096,
        };
        let d = |pcpus: u64, m2: u64, m1: u64, any: u64| {
            let mut d = Demand {
                pcpus,
                any_mib: any,
                ..Demand::default()
            };
            if m2 > 0 {
                d.pages.insert(PageKind::Size2M, m2);
            }
            if m1 > 0 {
                d.pages.insert(PageKind::Size1G, m1);
            }
            d
        };
        assert_eq!(exceeded(&limits, &d(3, 0, 0, 0), &d(4, 0, 0, 0)), None);
        let e = exceeded(&limits, &d(3, 0, 0, 0), &d(5, 0, 0, 0)).unwrap();
        assert_eq!(
            (e.resource, e.limit, e.used),
            (HostResource::DedicatedCpus, 4, 5)
        );
        assert_eq!(e.to_string(), "would use 5 of 4 dedicated CPUs");
        // 상한이 줄어 이미 넘친 노드: 늘리지 않는 쓰기는 된다.
        assert_eq!(exceeded(&limits, &d(6, 0, 0, 0), &d(6, 0, 0, 0)), None);
        assert_eq!(exceeded(&limits, &d(6, 0, 0, 0), &d(5, 0, 0, 0)), None);
        assert_eq!(
            exceeded(&limits, &d(0, 2048, 0, 0), &d(0, 2050, 0, 0))
                .unwrap()
                .resource,
            HostResource::Hugepages2M
        );
        assert_eq!(
            exceeded(&limits, &d(0, 0, 0, 0), &d(0, 0, 5120, 0))
                .unwrap()
                .resource,
            HostResource::Hugepages1G
        );
        // `any`는 두 크기 합에 센다.
        assert_eq!(
            exceeded(&limits, &d(0, 0, 0, 0), &d(0, 2048, 0, 4096)),
            None
        );
        let e = exceeded(&limits, &d(0, 2048, 0, 4096), &d(0, 2048, 0, 4098)).unwrap();
        assert_eq!(
            (e.resource, e.limit, e.used),
            (HostResource::Hugepages, 6144, 6146)
        );
        assert_eq!(e.to_string(), "would use 6146 of 6144 MiB of hugepages");
        assert_eq!(
            serde_json::to_value(HostResource::Hugepages2M).unwrap(),
            json!("hugepages_2m")
        );
    }

    #[test]
    fn demand_adds_and_compares() {
        let mut a = Demand {
            pcpus: 2,
            ..Demand::default()
        };
        a.pages.insert(PageKind::Size2M, 512);
        let mut b = a.clone();
        b.add(&a);
        assert_eq!((b.pcpus, b.mib(PageKind::Size2M)), (4, 1024));
        assert!(a.within(&b));
        assert!(!b.within(&a));
        assert!(Demand::default().within(&a));
        let mut c = Demand::default();
        c.pages.insert(PageKind::Size1G, 1024);
        assert!(!c.within(&b));
    }

    #[test]
    fn host_free_subtracts_usage() {
        let t = xeon();
        let sets = HostSets::new(Some(&resources("", "0-7")), &t);
        let mut used = HostUsage {
            dedicated: set("0-1"),
            ..HostUsage::default()
        };
        used.pages.insert((1, PageKind::Size2M), 600);
        let free = HostFree::new(&t, &sets, &used);
        assert_eq!(free.dedicated, set("2-7"));
        assert_eq!(free.pages[&(0, PageKind::Size2M)], 512);
        assert_eq!(free.pages[&(1, PageKind::Size2M)], 0);
        assert_eq!(free.pages[&(1, PageKind::Size1G)], 4);
        assert_eq!(t.nodes[0].small_memory_mib(), 65536 - 1024 - 4096);
    }

    // ---------------------------------------------------------------------------------------------------------
    // 보류
    // ---------------------------------------------------------------------------------------------------------

    fn hw_for(spec: &VmSpec) -> RunningHardware {
        let mut hw: RunningHardware = serde_json::from_value(json!({
            "cpu": { "sockets": 2, "cores": 4 },
            "memory_mib": 8192,
            "firmware": "uefi",
            "machine": "q35",
        }))
        .unwrap();
        hw.cpu = spec.cpu.clone();
        let t = xeon();
        hw.numa = layout(spec, Some(&t)).unwrap().map(|l| {
            let sets = HostSets::new(Some(&resources("", "0-15")), &t);
            let free = HostFree::new(&t, &sets, &HostUsage::default());
            let view = HostView {
                node: NODE,
                topology: &t,
                sets: &sets,
                free: &free,
            };
            fit(&l, Some(&view)).unwrap().numa
        });
        hw
    }

    #[test]
    fn pending_change_compares_spec_with_the_record() {
        let plain = vm(json!({}));
        let hw = hw_for(&plain);
        assert_eq!(pending_change(&plain, &hw), None);
        // 정책
        let ded = vm(json!({ "cpu": { "policy": "dedicated" } }));
        let p = pending_change(&ded, &hw).unwrap();
        assert_eq!(p.field, "numa");
        assert_eq!(p.reason, PendingReason::Restart);
        assert_eq!(p.running, "none");
        assert_eq!(p.wanted, "auto, dedicated, memory strict");
        let detail = p.detail.unwrap();
        assert!(detail.starts_with("shared → dedicated"), "{detail}");
        assert!(detail.ends_with(POWER_CYCLE), "{detail}");
        // 켠 그대로면 없음(auto는 어느 모양이든)
        let hw = hw_for(&ded);
        assert_eq!(pending_change(&ded, &hw), None);
        assert_eq!(
            describe(hw.numa.as_ref()),
            "1 node, dedicated, memory strict"
        );
        // 스레드·에뮬레이터
        let iso = vm(
            json!({ "cpu": { "policy": "dedicated", "thread_policy": "isolate", "emulator": "isolate" } }),
        );
        let d = pending_change(&iso, &hw).unwrap().detail.unwrap();
        assert!(
            d.contains("threads prefer → isolate")
                && d.contains("emulator threads shared → isolate"),
            "{d}"
        );
        // 페이지: any는 기록된 크기를 받는다.
        let any = vm(json!({ "hugepages": "any" }));
        let hw = hw_for(&any);
        assert_eq!(hw.numa.as_ref().unwrap().page, Some(PageKind::Size1G));
        assert_eq!(pending_change(&any, &hw), None);
        let two = vm(json!({ "hugepages": "2M" }));
        assert!(
            pending_change(&two, &hw)
                .unwrap()
                .detail
                .unwrap()
                .contains("hugepages 1G → 2M")
        );
        // 노드 수
        let n2 = vm(json!({ "numa": { "nodes": 2 } }));
        let hw = hw_for(&n2);
        assert_eq!(pending_change(&n2, &hw), None);
        let n1 = vm(json!({ "numa": { "nodes": 1 } }));
        assert!(
            pending_change(&n1, &hw)
                .unwrap()
                .detail
                .unwrap()
                .contains("NUMA 2 nodes → 1 node")
        );
        assert!(
            pending_change(&plain, &hw)
                .unwrap()
                .detail
                .unwrap()
                .contains("NUMA 2 nodes → none")
        );
        // 메모리 묶기
        let pref = vm(json!({ "numa": { "nodes": 2, "memory": "preferred" } }));
        assert!(
            pending_change(&pref, &hw)
                .unwrap()
                .detail
                .unwrap()
                .contains("memory none → preferred")
        );
        // 직접 목록
        let list = vm(
            json!({ "numa": { "nodes": [{ "cpus": 4, "memory_mib": 4096 }, { "cpus": 4, "memory_mib": 4096 }] } }),
        );
        assert_eq!(pending_change(&list, &hw), None);
        let list = vm(
            json!({ "numa": { "nodes": [{ "cpus": 4, "memory_mib": 6144 }, { "cpus": 4, "memory_mib": 2048 }] } }),
        );
        assert!(pending_change(&list, &hw).is_some());
    }
}
