//! 사용량 지표. 노드마다 5초마다 노드와 그 노드에서 도는 VM을 재고 메모리에 1시간 둔다(Raft에 쓰지 않는다).
//! 비율은 직전 측정과의 차이를 걸린 시간으로 나눈 값이고, 읽지 못한 값은 `null`이다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// 노드 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// CPU 사용률 %(모든 코어를 합해 100)
    pub cpu: Option<f64>,
    pub mem_used_mib: Option<u64>,
    pub mem_total_mib: Option<u64>,
    /// 실제 디스크의 읽기 바이트/초(macOS는 `null`)
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    /// 실제 NIC의 받기 바이트/초
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
    /// 데이터 디렉터리가 있는 파일 시스템
    pub fs_used_mib: Option<u64>,
    pub fs_total_mib: Option<u64>,
}

/// VM 점 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSample {
    /// 측정 시각(그 노드 벽시계의 유닉스 밀리초)
    pub t: i64,
    /// vCPU 대비 사용률 %(vCPU가 모두 바쁘면 100). QEMU 프로세스 전체라 100을 조금 넘을 수 있다.
    pub cpu: Option<f64>,
    /// QEMU 프로세스의 RSS
    pub mem_rss_mib: Option<u64>,
    /// 모든 디스크를 합한 값
    pub disk_read_bps: Option<f64>,
    pub disk_write_bps: Option<f64>,
    pub disk_read_iops: Option<f64>,
    pub disk_write_iops: Option<f64>,
    /// 게스트 기준 받기 바이트/초(tap이 없으면 `null`)
    pub net_rx_bps: Option<f64>,
    pub net_tx_bps: Option<f64>,
}

/// 한 노드에 묻는 지표
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct MetricsQuery {
    pub scope: MetricsScope,
    /// 이 `t`보다 뒤의 점만(그 노드 시계 기준). 없으면 남은 것 모두.
    pub since: Option<i64>,
}

/// 무엇의 지표인가
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MetricsScope {
    /// 그 노드 자신
    Nodes,
    /// 그 노드에서 잰 VM 하나
    Vm { id: Uuid },
}

/// 한 노드의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeSeries {
    pub node: Uuid,
    pub samples: Vec<NodeSample>,
}

/// 한 노드에서 잰 VM 하나의 점들(시각 순)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmSeries {
    pub vm: Uuid,
    /// 잰 노드
    pub node: Uuid,
    pub samples: Vec<VmSample>,
}

/// `GET /metrics/nodes`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NodeMetricsResponse {
    /// 읽은 노드마다 하나(노드 이름 순)
    pub nodes: Vec<NodeSeries>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

/// `GET /vms/{key}/metrics`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct VmMetricsResponse {
    pub vm: Uuid,
    /// 모든 노드에서 잰 점을 시각 순으로 합친 것(마이그레이션 앞뒤가 이어진다)
    pub samples: Vec<VmSample>,
    /// 답하지 않은 노드의 이름(정렬)
    pub unreachable: Vec<String>,
    /// 지표를 모르는 옛 노드의 이름(정렬)
    pub unsupported: Vec<String>,
}

/// 기록(계획 60)의 칸 하나. snake_case 이름이 API 열 이름이고, 단위는 점과 같다(MiB 칸은 기록에서도 MiB, 비율은
/// 초당 값, CPU는 %).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum MetricField {
    /// 노드·VM CPU %
    Cpu,
    /// 노드 메모리(MiB)
    MemUsed,
    MemTotal,
    /// 노드·VM 디스크 바이트/초
    DiskRead,
    DiskWrite,
    /// 노드·VM 네트워크 바이트/초
    NetRx,
    NetTx,
    /// 노드 데이터 디렉터리 파일 시스템(MiB)
    FsUsed,
    FsTotal,
    /// VM QEMU 프로세스 RSS(MiB)
    MemRss,
    /// VM 디스크 IOPS
    DiskReadIops,
    DiskWriteIops,
    /// 풀 사용량·전체(MiB). `PoolState`의 바이트를 MiB로 줄여 둔다(f32 칸에 맞게).
    PoolUsed,
    PoolTotal,
}

impl MetricField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::MemUsed => "mem_used",
            Self::MemTotal => "mem_total",
            Self::DiskRead => "disk_read",
            Self::DiskWrite => "disk_write",
            Self::NetRx => "net_rx",
            Self::NetTx => "net_tx",
            Self::FsUsed => "fs_used",
            Self::FsTotal => "fs_total",
            Self::MemRss => "mem_rss",
            Self::DiskReadIops => "disk_read_iops",
            Self::DiskWriteIops => "disk_write_iops",
            Self::PoolUsed => "pool_used",
            Self::PoolTotal => "pool_total",
        }
    }
}

impl std::fmt::Display for MetricField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 노드 기록의 칸(이 순서가 [`NodeSample::values`]의 순서다)
pub const NODE_FIELDS: [MetricField; 9] = [
    MetricField::Cpu,
    MetricField::MemUsed,
    MetricField::MemTotal,
    MetricField::DiskRead,
    MetricField::DiskWrite,
    MetricField::NetRx,
    MetricField::NetTx,
    MetricField::FsUsed,
    MetricField::FsTotal,
];

/// VM 기록의 칸(이 순서가 [`VmSample::values`]의 순서다)
pub const VM_FIELDS: [MetricField; 8] = [
    MetricField::Cpu,
    MetricField::MemRss,
    MetricField::DiskRead,
    MetricField::DiskWrite,
    MetricField::DiskReadIops,
    MetricField::DiskWriteIops,
    MetricField::NetRx,
    MetricField::NetTx,
];

/// 풀 기록의 칸(이 순서가 [`pool_values`]의 순서다)
pub const POOL_FIELDS: [MetricField; 2] = [MetricField::PoolUsed, MetricField::PoolTotal];

const MIB: f64 = 1024.0 * 1024.0;

fn f(v: Option<f64>) -> Option<f32> {
    v.filter(|v| v.is_finite()).map(|v| v as f32)
}

fn m(v: Option<u64>) -> Option<f32> {
    v.map(|v| v as f32)
}

impl NodeSample {
    /// [`NODE_FIELDS`] 순서의 값(유한하지 않은 값은 `None`)
    pub fn values(&self) -> [Option<f32>; 9] {
        [
            f(self.cpu),
            m(self.mem_used_mib),
            m(self.mem_total_mib),
            f(self.disk_read_bps),
            f(self.disk_write_bps),
            f(self.net_rx_bps),
            f(self.net_tx_bps),
            m(self.fs_used_mib),
            m(self.fs_total_mib),
        ]
    }

    /// [`NodeSample::values`]를 되돌린다(기록 파일의 `Raw5s` 칸을 메모리 저장소로 되살릴 때). 값은 [`NODE_FIELDS`]
    /// 순서이고, MiB 칸은 반올림한다. 모자란 값은 `None`이다.
    pub fn from_values(t: i64, values: &[Option<f32>]) -> NodeSample {
        let v = |i: usize| values.get(i).copied().flatten().filter(|v| v.is_finite());
        NodeSample {
            t,
            cpu: v(0).map(f64::from),
            mem_used_mib: v(1).map(to_mib),
            mem_total_mib: v(2).map(to_mib),
            disk_read_bps: v(3).map(f64::from),
            disk_write_bps: v(4).map(f64::from),
            net_rx_bps: v(5).map(f64::from),
            net_tx_bps: v(6).map(f64::from),
            fs_used_mib: v(7).map(to_mib),
            fs_total_mib: v(8).map(to_mib),
        }
    }
}

/// f32 MiB 값을 정수 MiB로(음수는 0)
fn to_mib(v: f32) -> u64 {
    v.max(0.0).round() as u64
}

impl VmSample {
    /// [`VM_FIELDS`] 순서의 값(유한하지 않은 값은 `None`)
    pub fn values(&self) -> [Option<f32>; 8] {
        [
            f(self.cpu),
            m(self.mem_rss_mib),
            f(self.disk_read_bps),
            f(self.disk_write_bps),
            f(self.disk_read_iops),
            f(self.disk_write_iops),
            f(self.net_rx_bps),
            f(self.net_tx_bps),
        ]
    }

    /// [`VmSample::values`]를 되돌린다([`NodeSample::from_values`]와 같다). 값은 [`VM_FIELDS`] 순서다.
    pub fn from_values(t: i64, values: &[Option<f32>]) -> VmSample {
        let v = |i: usize| values.get(i).copied().flatten().filter(|v| v.is_finite());
        VmSample {
            t,
            cpu: v(0).map(f64::from),
            mem_rss_mib: v(1).map(to_mib),
            disk_read_bps: v(2).map(f64::from),
            disk_write_bps: v(3).map(f64::from),
            disk_read_iops: v(4).map(f64::from),
            disk_write_iops: v(5).map(f64::from),
            net_rx_bps: v(6).map(f64::from),
            net_tx_bps: v(7).map(f64::from),
        }
    }
}

/// 풀의 [`POOL_FIELDS`] 순서 값(MiB): 쓴 양(`total - available`)과 전체
pub fn pool_values(total_bytes: u64, available_bytes: u64) -> [Option<f32>; 2] {
    let used = total_bytes.saturating_sub(available_bytes);
    [
        Some((used as f64 / MIB) as f32),
        Some((total_bytes as f64 / MIB) as f32),
    ]
}

/// 기록 층. 칸 `b = floor(t / step)`(유닉스 기준)라서 모든 노드의 칸 경계가 같다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 5초 × 720 = 1시간(값 하나, AVG=MAX=MIN)
    Raw5s,
    /// 1분 × 1440 = 1일
    Min1,
    /// 30분 × 336 = 1주
    Min30,
    /// 6시간 × 1460 = 1년
    Hour6,
}

/// 고운 층부터
pub const TIERS: [Tier; 4] = [Tier::Raw5s, Tier::Min1, Tier::Min30, Tier::Hour6];

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Raw5s => "raw5s",
            Self::Min1 => "min1",
            Self::Min30 => "min30",
            Self::Hour6 => "hour6",
        }
    }

    /// 칸 하나의 길이(ms)
    pub fn step_ms(self) -> i64 {
        match self {
            Self::Raw5s => 5_000,
            Self::Min1 => 60_000,
            Self::Min30 => 30 * 60_000,
            Self::Hour6 => 6 * 3_600_000,
        }
    }

    /// 칸 수
    pub fn rows(self) -> u32 {
        match self {
            Self::Raw5s => 720,
            Self::Min1 => 1440,
            Self::Min30 => 336,
            Self::Hour6 => 1460,
        }
    }

    /// 이 층이 담는 기간(ms) = 간격 × 칸 수
    pub fn span_ms(self) -> i64 {
        self.step_ms() * i64::from(self.rows())
    }

    /// 한 층 거친 층(가장 거친 층은 자기 자신)
    pub fn coarser(self) -> Tier {
        match self {
            Self::Raw5s => Self::Min1,
            Self::Min1 => Self::Min30,
            Self::Min30 | Self::Hour6 => Self::Hour6,
        }
    }
}

/// 조회 범위 이름(`range=`). 끝은 지금이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum HistoryRange {
    Hour,
    Day,
    Week,
    /// 30일
    Month,
    /// 365일
    Year,
}

impl HistoryRange {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    /// 범위의 길이(ms)
    pub fn ms(self) -> i64 {
        const DAY: i64 = 86_400_000;
        match self {
            Self::Hour => 3_600_000,
            Self::Day => DAY,
            Self::Week => 7 * DAY,
            Self::Month => 30 * DAY,
            Self::Year => 365 * DAY,
        }
    }

    /// `now`에서 끝나는 `(from, until)`
    pub fn window(self, now: i64) -> (i64, i64) {
        (now - self.ms(), now)
    }
}

/// `points`의 기본값과 최댓값
pub const DEFAULT_HISTORY_POINTS: u16 = 300;
pub const MAX_HISTORY_POINTS: u16 = 1000;
/// 클러스터 모으기의 점 수와 노드마다 돌려주는 상위 VM 수
pub const DIGEST_POINTS: u16 = 120;
pub const DIGEST_TOP: u8 = 10;
/// `Digest`의 `top` 최댓값
pub const MAX_DIGEST_TOP: u8 = 50;
/// 조회 범위의 최대 길이(ms, 366일). 가장 거친 층(1년)보다 조금 길다.
pub const MAX_HISTORY_RANGE_MS: i64 = 366 * 86_400_000;

/// 한 노드에 묻는 기록(peer RPC `ReadMetricsHistory`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HistoryQuery {
    pub scope: HistoryScope,
    /// 범위 시작·끝(유닉스 ms, `from < until`)
    pub from: i64,
    pub until: i64,
    /// 돌려줄 점 수의 상한(1~1000)
    pub points: u16,
}

/// 무엇의 기록인가
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryScope {
    /// 그 노드 자신
    Node,
    /// 그 노드가 가진 VM 하나
    Vm { id: Uuid },
    /// 그 노드에서 잰 풀 하나(풀 id)
    Pool { id: Uuid },
    /// 클러스터 모으기: 노드 열의 AVG와 범위 평균 CPU 상위 VM `top`개
    Digest { top: u8 },
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HistoryQueryError {
    #[error("from {from} must be before until {until}")]
    Order { from: i64, until: i64 },
    #[error("the range is too long: at most {MAX_HISTORY_RANGE_MS} ms (366 days)")]
    TooLong,
    #[error("points {0} is out of range: use 1-{MAX_HISTORY_POINTS}")]
    Points(u16),
    #[error("top {0} is out of range: use 1-{MAX_DIGEST_TOP}")]
    Top(u8),
}

impl HistoryQuery {
    /// 범위·점 수·`top`을 본다.
    pub fn validate(&self) -> Result<(), HistoryQueryError> {
        if self.from >= self.until {
            return Err(HistoryQueryError::Order {
                from: self.from,
                until: self.until,
            });
        }
        if self.until.saturating_sub(self.from) > MAX_HISTORY_RANGE_MS {
            return Err(HistoryQueryError::TooLong);
        }
        if !(1..=MAX_HISTORY_POINTS).contains(&self.points) {
            return Err(HistoryQueryError::Points(self.points));
        }
        if let HistoryScope::Digest { top } = self.scope
            && !(1..=MAX_DIGEST_TOP).contains(&top)
        {
            return Err(HistoryQueryError::Top(top));
        }
        Ok(())
    }
}

/// 열 하나의 AVG·MAX·MIN. 모르는 칸(NaN)은 `null`이다. `Raw5s`는 세 열이 같고, `Digest`는 AVG만 있다(MAX·MIN은
/// 비어 있고 쓰지 않는다).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Cf {
    pub avg: Vec<Option<f32>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub max: Vec<Option<f32>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub min: Vec<Option<f32>>,
}

/// 범위 전체의 요약. 아는 칸이 없으면 `null`이다.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Stat {
    /// 아는 칸 수로 가중한 평균
    pub avg: Option<f32>,
    /// MAX 열의 최대
    pub max: Option<f32>,
    /// MIN 열의 최소
    pub min: Option<f32>,
}

/// 백분위(최근순위)
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct Pct {
    pub p50: Option<f32>,
    pub p95: Option<f32>,
    /// 5초 원본이 아니라 층의 AVG 값으로 셈한 근사(짧은 봉우리를 낮게 본다)
    pub approx: bool,
}

/// `Digest`의 상위 VM 하나(범위 평균)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct TopVm {
    pub vm: Uuid,
    /// 범위 평균 CPU %
    pub cpu: Option<f32>,
    /// 범위 평균 RSS(MiB)
    pub mem_rss: Option<f32>,
}

/// 기록 조회의 답. 열은 웹 uPlot이 그대로 받는 배열이고, 점 `i`의 시각은 `start + i × step_ms`(칸 시작)다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct HistoryReply {
    pub tier: Tier,
    /// 점 하나의 길이(층 간격 × 묶은 칸 수)
    pub step_ms: i64,
    /// 첫 점의 시각(유닉스 ms)
    pub start: i64,
    /// 칸 이름([`MetricField::as_str`]) → 열
    pub columns: BTreeMap<String, Cf>,
    /// 칸 이름 → 범위 요약
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub stats: BTreeMap<String, Stat>,
    /// 칸 이름 → p50·p95
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub pct: BTreeMap<String, Pct>,
    /// 기록이 범위 시작보다 늦게 시작하면 실제로 아는 가장 이른 시각(재시작 직후·마이그레이션 직후)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub covered_from: Option<i64>,
    /// `Digest`의 범위 평균 CPU 상위 VM(내림차순)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub top: Vec<TopVm>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_tagged_by_kind() {
        let id = Uuid::now_v7();
        let q = MetricsQuery {
            scope: MetricsScope::Vm { id },
            since: Some(5),
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"scope": {"kind": "vm", "id": id}, "since": 5})
        );
        assert_eq!(serde_json::from_value::<MetricsQuery>(json).unwrap(), q);
        let nodes = serde_json::json!({"scope": {"kind": "nodes"}, "since": null});
        assert_eq!(
            serde_json::from_value::<MetricsQuery>(nodes).unwrap(),
            MetricsQuery {
                scope: MetricsScope::Nodes,
                since: None
            }
        );
    }

    /// 읽지 못한 값은 빼지 않고 `null`로 보낸다(웹이 차트를 끊어 그린다).
    #[test]
    fn missing_values_are_null() {
        let series = VmSeries {
            vm: Uuid::now_v7(),
            node: Uuid::now_v7(),
            samples: vec![VmSample {
                t: 1_000,
                cpu: Some(12.5),
                mem_rss_mib: Some(512),
                disk_read_bps: None,
                disk_write_bps: None,
                disk_read_iops: None,
                disk_write_iops: None,
                net_rx_bps: Some(0.0),
                net_tx_bps: None,
            }],
        };
        let json = serde_json::to_value(&series).unwrap();
        assert_eq!(json["samples"][0]["cpu"], 12.5);
        assert_eq!(json["samples"][0]["disk_read_bps"], serde_json::Value::Null);
        assert_eq!(json["samples"][0]["net_rx_bps"], 0.0);
        assert_eq!(serde_json::from_value::<VmSeries>(json).unwrap(), series);
    }

    #[test]
    fn field_names_are_api_column_names() {
        for f in NODE_FIELDS.iter().chain(&VM_FIELDS).chain(&POOL_FIELDS) {
            assert_eq!(serde_json::to_value(f).unwrap(), f.as_str());
            assert_eq!(f.to_string(), f.as_str());
        }
        assert_eq!(MetricField::DiskReadIops.as_str(), "disk_read_iops");
        assert_eq!(MetricField::PoolUsed.as_str(), "pool_used");
    }

    #[test]
    fn sample_values_follow_field_order() {
        let n = NodeSample {
            t: 0,
            cpu: Some(1.0),
            mem_used_mib: Some(2),
            mem_total_mib: Some(3),
            disk_read_bps: Some(4.0),
            disk_write_bps: Some(f64::NAN),
            net_rx_bps: Some(6.0),
            net_tx_bps: None,
            fs_used_mib: Some(8),
            fs_total_mib: Some(9),
        };
        assert_eq!(
            n.values(),
            [
                Some(1.0),
                Some(2.0),
                Some(3.0),
                Some(4.0),
                None,
                Some(6.0),
                None,
                Some(8.0),
                Some(9.0)
            ]
        );
        let v = VmSample {
            t: 0,
            cpu: Some(1.0),
            mem_rss_mib: Some(2),
            disk_read_bps: Some(3.0),
            disk_write_bps: Some(4.0),
            disk_read_iops: Some(5.0),
            disk_write_iops: Some(6.0),
            net_rx_bps: Some(7.0),
            net_tx_bps: Some(f64::INFINITY),
        };
        assert_eq!(
            v.values(),
            [
                Some(1.0),
                Some(2.0),
                Some(3.0),
                Some(4.0),
                Some(5.0),
                Some(6.0),
                Some(7.0),
                None
            ]
        );
        let gib = 1024 * 1024 * 1024;
        assert_eq!(
            pool_values(10 * gib, 4 * gib),
            [Some(6.0 * 1024.0), Some(10.0 * 1024.0)]
        );
        assert_eq!(pool_values(1, 5), [Some(0.0), Some(1.0 / 1048576.0)]);
    }

    /// 기록 파일에서 되살린 값은 같은 점이 된다(MiB는 반올림, 모르는 값은 `None`).
    #[test]
    fn samples_come_back_from_their_values() {
        let n = NodeSample {
            t: 5_000,
            cpu: Some(12.5),
            mem_used_mib: Some(2_048),
            mem_total_mib: Some(4_096),
            disk_read_bps: Some(1_000.0),
            disk_write_bps: None,
            net_rx_bps: Some(0.0),
            net_tx_bps: Some(3.0),
            fs_used_mib: Some(7),
            fs_total_mib: None,
        };
        assert_eq!(NodeSample::from_values(5_000, &n.values()), n);
        let v = VmSample {
            t: 10_000,
            cpu: Some(50.0),
            mem_rss_mib: Some(512),
            disk_read_bps: None,
            disk_write_bps: Some(8.0),
            disk_read_iops: Some(1.0),
            disk_write_iops: None,
            net_rx_bps: Some(2.0),
            net_tx_bps: Some(4.0),
        };
        assert_eq!(VmSample::from_values(10_000, &v.values()), v);
        // 반올림·음수·NaN·모자란 값
        let short = NodeSample::from_values(0, &[Some(f32::NAN), Some(1.6), Some(-3.0)]);
        assert_eq!(short.cpu, None);
        assert_eq!(short.mem_used_mib, Some(2));
        assert_eq!(short.mem_total_mib, Some(0));
        assert_eq!(short.fs_total_mib, None);
    }

    #[test]
    fn tiers_hold_their_spans() {
        assert_eq!(Tier::Raw5s.span_ms(), 3_600_000);
        assert_eq!(Tier::Min1.span_ms(), 86_400_000);
        assert_eq!(Tier::Min30.span_ms(), 7 * 86_400_000);
        assert_eq!(Tier::Hour6.span_ms(), 365 * 86_400_000);
        assert_eq!(Tier::Raw5s.coarser(), Tier::Min1);
        assert_eq!(Tier::Hour6.coarser(), Tier::Hour6);
        for t in TIERS {
            assert_eq!(serde_json::to_value(t).unwrap(), t.as_str());
        }
        assert_eq!(
            HistoryRange::Week.window(1_000_000_000),
            (1_000_000_000 - 7 * 86_400_000, 1_000_000_000)
        );
        for r in [
            HistoryRange::Hour,
            HistoryRange::Day,
            HistoryRange::Week,
            HistoryRange::Month,
            HistoryRange::Year,
        ] {
            assert_eq!(serde_json::to_value(r).unwrap(), r.as_str());
        }
    }

    #[test]
    fn history_query_is_tagged_and_validated() {
        let id = Uuid::now_v7();
        let q = HistoryQuery {
            scope: HistoryScope::Pool { id },
            from: 0,
            until: 3_600_000,
            points: 300,
        };
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"scope": {"kind": "pool", "id": id}, "from": 0, "until": 3_600_000, "points": 300})
        );
        assert_eq!(serde_json::from_value::<HistoryQuery>(json).unwrap(), q);
        assert_eq!(
            serde_json::to_value(HistoryScope::Digest { top: 10 }).unwrap(),
            serde_json::json!({"kind": "digest", "top": 10})
        );
        assert_eq!(
            serde_json::to_value(HistoryScope::Node).unwrap(),
            serde_json::json!({"kind": "node"})
        );
        assert_eq!(q.validate(), Ok(()));
        let bad = |f: fn(&mut HistoryQuery)| {
            let mut q = q.clone();
            f(&mut q);
            q.validate()
        };
        assert!(matches!(
            bad(|q| q.until = 0),
            Err(HistoryQueryError::Order { .. })
        ));
        assert_eq!(bad(|q| q.points = 0), Err(HistoryQueryError::Points(0)));
        assert_eq!(
            bad(|q| q.points = 1001),
            Err(HistoryQueryError::Points(1001))
        );
        assert_eq!(bad(|q| q.points = 1000), Ok(()));
        assert_eq!(
            bad(|q| q.from = q.until - MAX_HISTORY_RANGE_MS - 1),
            Err(HistoryQueryError::TooLong)
        );
        assert_eq!(bad(|q| q.from = i64::MIN), Err(HistoryQueryError::TooLong));
        assert_eq!(
            bad(|q| q.scope = HistoryScope::Digest { top: 0 }),
            Err(HistoryQueryError::Top(0))
        );
        assert_eq!(
            bad(|q| q.scope = HistoryScope::Digest { top: 51 }),
            Err(HistoryQueryError::Top(51))
        );
    }

    /// 모르는 칸은 `null`, 비어 있는 요약·백분위·`covered_from`·`top`은 쓰지 않는다.
    #[test]
    fn history_reply_json() {
        let reply = HistoryReply {
            tier: Tier::Min1,
            step_ms: 300_000,
            start: 60_000,
            columns: BTreeMap::from([(
                "cpu".to_string(),
                Cf {
                    avg: vec![Some(1.5), None],
                    max: vec![Some(2.0), None],
                    min: vec![Some(1.0), None],
                },
            )]),
            stats: BTreeMap::new(),
            pct: BTreeMap::new(),
            covered_from: None,
            top: vec![],
        };
        let json = serde_json::to_value(&reply).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "tier": "min1", "step_ms": 300_000, "start": 60_000,
                "columns": {"cpu": {"avg": [1.5, null], "max": [2.0, null], "min": [1.0, null]}}
            })
        );
        assert_eq!(serde_json::from_value::<HistoryReply>(json).unwrap(), reply);
        let digest = HistoryReply {
            columns: BTreeMap::from([(
                "cpu".to_string(),
                Cf {
                    avg: vec![Some(1.0)],
                    ..Cf::default()
                },
            )]),
            stats: BTreeMap::from([("cpu".to_string(), Stat::default())]),
            pct: BTreeMap::from([(
                "cpu".to_string(),
                Pct {
                    p50: Some(1.0),
                    p95: None,
                    approx: true,
                },
            )]),
            covered_from: Some(5),
            top: vec![TopVm {
                vm: Uuid::nil(),
                cpu: Some(3.0),
                mem_rss: None,
            }],
            ..reply
        };
        let json = serde_json::to_value(&digest).unwrap();
        assert_eq!(json["columns"]["cpu"], serde_json::json!({"avg": [1.0]}));
        assert_eq!(
            json["stats"]["cpu"],
            serde_json::json!({"avg": null, "max": null, "min": null})
        );
        assert_eq!(
            json["pct"]["cpu"],
            serde_json::json!({"p50": 1.0, "p95": null, "approx": true})
        );
        assert_eq!(json["covered_from"], 5);
        assert_eq!(json["top"][0]["cpu"], 3.0);
        assert_eq!(
            serde_json::from_value::<HistoryReply>(json).unwrap(),
            digest
        );
    }
}
