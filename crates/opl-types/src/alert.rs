//! 경보 규칙과 알림 대상(계획 60 설계 §8·§9). 규칙(`AlertRule`)은 지표 임계값·지속 시간이나 이벤트를 보고, 대상
//! (`NotificationTarget`, admin만 읽음)은 SMTP·webhook 연결 정보와 비밀을 담는다. 경보는 그 일이 일어난 노드가
//! 평가하고(노드 경보, 노드 파일), `NodeOffline`·`BackupJobFailed`만 리더가 평가해 `AlertRule.status.active`에 둔다.
//! 시각은 모두 유닉스 ms다.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::metrics::{NodeSample, VmSample};
use crate::resource::{Kind, NameError, Resource, ResourceSpec, validate_name};

pub mod sdn;

pub type AlertRule = Resource<AlertRuleSpec, AlertRuleStatus>;
pub type NotificationTarget = Resource<NotificationTargetSpec, NotificationTargetStatus>;

/// `for_secs` 최댓값(1일)
pub const MAX_FOR_SECS: u32 = 86_400;
/// `repeat_secs` 기본값(4시간)과 범위(0이면 다시 보내지 않음, 아니면 1분~7일)
pub const DEFAULT_REPEAT_SECS: u32 = 14_400;
pub const MIN_REPEAT_SECS: u32 = 60;
pub const MAX_REPEAT_SECS: u32 = 7 * 86_400;
/// 규칙 하나의 대상 수 상한
pub const MAX_RULE_TARGETS: usize = 16;
/// 규칙의 `nodes`·`vms` 목록 상한
pub const MAX_RULE_SUBJECTS: usize = 256;
/// `TaskFailed.ops` 상한
pub const MAX_RULE_OPS: usize = 64;
/// 조건이 거짓인 채 이만큼 지나야 해제한다(깜박임 막기)
pub const RESOLVE_AFTER_MS: i64 = 60_000;
/// SMTP 받는 사람 수 상한
pub const MAX_SMTP_RECIPIENTS: usize = 20;
/// webhook 헤더 수 상한
pub const MAX_WEBHOOK_HEADERS: usize = 16;

// ---- 규칙 ----

/// 경보 규칙 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct AlertRuleSpec {
    /// 끄면 평가하지 않고, 활성 경보는 알림 없이 해제한다.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub enabled: bool,
    pub condition: AlertCondition,
    /// 조건이 이만큼(초) 참이어야 알린다(0~86400). 이벤트 조건은 0이다.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default = 0, maximum = 86400))]
    pub for_secs: u32,
    pub severity: Severity,
    /// 알릴 `NotificationTarget` 이름(1~16개)
    pub targets: Vec<String>,
    /// 경보가 이어지는 동안 이만큼(초)마다 다시 알린다. 0이면 다시 보내지 않는다.
    #[serde(
        default = "default_repeat_secs",
        skip_serializing_if = "is_default_repeat"
    )]
    #[cfg_attr(feature = "openapi", schema(default = 14400))]
    pub repeat_secs: u32,
    /// 해제도 알린다(이벤트 조건은 해제가 없다).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub send_resolved: bool,
    /// 이 노드(이름)들만. 비면 모든 노드
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub nodes: Vec<String>,
    /// 이 VM(이름)들만(VM 조건만). 비면 모든 VM
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub vms: Vec<String>,
}

impl ResourceSpec for AlertRuleSpec {
    const KIND: Kind = Kind::AlertRule;
    type Status = AlertRuleStatus;
}

/// 리더가 평가하는 규칙(`NodeOffline`·`BackupJobFailed`)의 활성 경보만 둔다. 노드 경보는 각 노드 파일에 있다.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct AlertRuleStatus {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub active: Vec<ActiveAlert>,
    /// `BackupJobFailed`: 백업 작업(id)마다 이미 알린 마지막 실패 실행의 끝난 시각(ms). 리더가 바뀌어도 같은 실행을 다시
    /// 알리지 않는다.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub notified: BTreeMap<Uuid, i64>,
}

/// 심각도(낮은 것부터)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 비교
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlertOp {
    /// 값 > 문턱
    Gt,
    /// 값 < 문턱
    Lt,
}

impl AlertOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gt => "gt",
            Self::Lt => "lt",
        }
    }

    /// `value`가 문턱 `threshold`를 넘었는지
    pub fn holds(self, value: f64, threshold: f64) -> bool {
        match self {
            Self::Gt => value > threshold,
            Self::Lt => value < threshold,
        }
    }
}

/// 노드 지표 규칙의 값
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NodeAlertMetric {
    /// CPU %
    Cpu,
    /// 메모리 사용률 %
    MemPercent,
    /// 데이터 디렉터리 파일 시스템 사용률 %
    FsPercent,
    /// 바이트/초
    DiskRead,
    DiskWrite,
    NetRx,
    NetTx,
}

impl NodeAlertMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::MemPercent => "mem_percent",
            Self::FsPercent => "fs_percent",
            Self::DiskRead => "disk_read",
            Self::DiskWrite => "disk_write",
            Self::NetRx => "net_rx",
            Self::NetTx => "net_tx",
        }
    }

    /// 점 하나에서 이 값. 모르면(또는 전체가 0이면) `None`
    pub fn read(self, s: &NodeSample) -> Option<f64> {
        let v = match self {
            Self::Cpu => s.cpu,
            Self::MemPercent => percent(s.mem_used_mib, s.mem_total_mib),
            Self::FsPercent => percent(s.fs_used_mib, s.fs_total_mib),
            Self::DiskRead => s.disk_read_bps,
            Self::DiskWrite => s.disk_write_bps,
            Self::NetRx => s.net_rx_bps,
            Self::NetTx => s.net_tx_bps,
        };
        v.filter(|v| v.is_finite())
    }
}

fn percent(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0 => Some(used as f64 * 100.0 / total as f64),
        _ => None,
    }
}

/// VM 지표 규칙의 값
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum VmAlertMetric {
    /// vCPU 대비 %
    Cpu,
    /// QEMU RSS(MiB)
    MemRss,
    /// 바이트/초
    DiskRead,
    DiskWrite,
    DiskReadIops,
    DiskWriteIops,
    NetRx,
    NetTx,
}

impl VmAlertMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::MemRss => "mem_rss",
            Self::DiskRead => "disk_read",
            Self::DiskWrite => "disk_write",
            Self::DiskReadIops => "disk_read_iops",
            Self::DiskWriteIops => "disk_write_iops",
            Self::NetRx => "net_rx",
            Self::NetTx => "net_tx",
        }
    }

    /// 점 하나에서 이 값. 모르면 `None`
    pub fn read(self, s: &VmSample) -> Option<f64> {
        let v = match self {
            Self::Cpu => s.cpu,
            Self::MemRss => s.mem_rss_mib.map(|v| v as f64),
            Self::DiskRead => s.disk_read_bps,
            Self::DiskWrite => s.disk_write_bps,
            Self::DiskReadIops => s.disk_read_iops,
            Self::DiskWriteIops => s.disk_write_iops,
            Self::NetRx => s.net_rx_bps,
            Self::NetTx => s.net_tx_bps,
        };
        v.filter(|v| v.is_finite())
    }
}

/// 경보 조건
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AlertCondition {
    /// 노드 지표가 문턱을 넘음(그 노드가 5초 점마다 평가)
    NodeMetric {
        metric: NodeAlertMetric,
        op: AlertOp,
        value: f64,
    },
    /// VM 지표가 문턱을 넘음(VM의 노드가 평가)
    VmMetric {
        metric: VmAlertMetric,
        op: AlertOp,
        value: f64,
    },
    /// 풀 사용률(%)이 `percent`를 넘음(로컬 풀은 그 노드, 공유 풀은 `ready`인 온라인 노드 가운데 id가 가장 작은
    /// 노드가 평가)
    PoolUsage { percent: f64 },
    /// 노드가 오프라인(리더가 평가)
    NodeOffline,
    /// VM `phase: failed`(VM의 노드가 평가)
    VmFailed,
    /// Task가 실패함(이벤트, Task의 노드가 평가). `ops`(Task 종류, 예: `vm-start`)가 비면 모든 Task
    TaskFailed {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[cfg_attr(feature = "openapi", schema(default))]
        ops: Vec<String>,
    },
    /// HA 복구 Task(`vm-ha-recover`)가 끝남(이벤트)
    HaRecovery,
    /// 백업 작업 실행이 실패함(이벤트, 리더가 평가)
    BackupJobFailed,
    /// SDN 중앙(OVN NB·SB) 정족수를 잃음(계획 67, 리더가 평가, 대상 SDN)
    SdnQuorumLost,
    /// 중앙 노드의 NB·SB가 꺼짐·후보이거나 노드가 오프라인(계획 67, 리더, 대상 노드)
    SdnCentralDown,
    /// chassis로 구성된 온라인 노드의 ovn-controller가 꺼짐·구성 중(계획 67, 리더, 대상 노드)
    SdnChassisDown,
    /// 바깥이 있는 라우터에 게이트웨이 후보가 없거나 모두 오프라인(계획 67, 리더, 대상 라우터)
    SdnGatewayUnavailable,
    /// 라우터의 활성 게이트웨이 chassis가 바뀜(이벤트, 계획 67, 리더, 대상 라우터)
    SdnGatewayFailover,
    /// OVN NB 동기화가 실패하거나 멈춤, 또는 잠금 표가 맞지 않는 노드가 있음(계획 67, 리더, 대상 SDN)
    SdnSyncFailed,
    /// 설정한 physnet(옮기기 다리 포함)이 준비되지 않음(계획 67, 리더, 대상 노드)
    SdnPhysnetNotReady,
    /// 부하 분산기의 건강 검사 멤버가 죽음(계획 67, 리더, 대상 부하 분산기)
    SdnLbMemberDown,
    /// 백업의 마지막 검증이 실패함(계획 68, 리더, 대상 백업 — 다시 검증해 통과하거나 백업을 지우면 풀림)
    BackupVerifyFailed,
    /// 백업 저장소에 닿지 않음(계획 68, 리더가 노드 보고 `backup_stores`로 평가, 대상 저장소)
    BackupStoreUnavailable,
}

/// 누가 평가하나
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evaluator {
    /// 그 일이 일어난 노드(노드 파일에 상태)
    Node,
    /// 리더(`AlertRule.status.active`에 상태)
    Leader,
}

impl AlertCondition {
    /// `kind` 태그 값
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NodeMetric { .. } => "node_metric",
            Self::VmMetric { .. } => "vm_metric",
            Self::PoolUsage { .. } => "pool_usage",
            Self::NodeOffline => "node_offline",
            Self::VmFailed => "vm_failed",
            Self::TaskFailed { .. } => "task_failed",
            Self::HaRecovery => "ha_recovery",
            Self::BackupJobFailed => "backup_job_failed",
            Self::SdnQuorumLost => "sdn_quorum_lost",
            Self::SdnCentralDown => "sdn_central_down",
            Self::SdnChassisDown => "sdn_chassis_down",
            Self::SdnGatewayUnavailable => "sdn_gateway_unavailable",
            Self::SdnGatewayFailover => "sdn_gateway_failover",
            Self::SdnSyncFailed => "sdn_sync_failed",
            Self::SdnPhysnetNotReady => "sdn_physnet_not_ready",
            Self::SdnLbMemberDown => "sdn_lb_member_down",
            Self::BackupVerifyFailed => "backup_verify_failed",
            Self::BackupStoreUnavailable => "backup_store_unavailable",
        }
    }

    /// 한 번 보내고 끝나는 이벤트 조건인지(지속 시간·반복·해제가 없다)
    pub fn is_event(&self) -> bool {
        matches!(
            self,
            Self::TaskFailed { .. }
                | Self::HaRecovery
                | Self::BackupJobFailed
                | Self::SdnGatewayFailover
        )
    }

    /// SDN 조건(계획 67 — 기능 `sdn-services`)인지
    pub fn is_sdn(&self) -> bool {
        matches!(
            self,
            Self::SdnQuorumLost
                | Self::SdnCentralDown
                | Self::SdnChassisDown
                | Self::SdnGatewayUnavailable
                | Self::SdnGatewayFailover
                | Self::SdnSyncFailed
                | Self::SdnPhysnetNotReady
                | Self::SdnLbMemberDown
        )
    }

    /// 백업 저장소 조건(계획 68 — 기능 `backup-store`)인지
    pub fn is_backup_store(&self) -> bool {
        matches!(
            self,
            Self::BackupVerifyFailed | Self::BackupStoreUnavailable
        )
    }

    pub fn evaluator(&self) -> Evaluator {
        match self {
            Self::NodeOffline | Self::BackupJobFailed => Evaluator::Leader,
            c if c.is_sdn() || c.is_backup_store() => Evaluator::Leader,
            _ => Evaluator::Node,
        }
    }

    /// 경보 대상의 종류
    pub fn subject_kind(&self) -> SubjectKind {
        match self {
            Self::NodeMetric { .. }
            | Self::NodeOffline
            | Self::SdnCentralDown
            | Self::SdnChassisDown
            | Self::SdnPhysnetNotReady => SubjectKind::Node,
            Self::SdnQuorumLost | Self::SdnSyncFailed => SubjectKind::Sdn,
            Self::SdnGatewayUnavailable | Self::SdnGatewayFailover => SubjectKind::Router,
            Self::SdnLbMemberDown => SubjectKind::LoadBalancer,
            Self::VmMetric { .. } | Self::VmFailed => SubjectKind::Vm,
            Self::PoolUsage { .. } => SubjectKind::Pool,
            Self::TaskFailed { .. } | Self::HaRecovery => SubjectKind::Task,
            Self::BackupJobFailed => SubjectKind::BackupJob,
            Self::BackupVerifyFailed => SubjectKind::Backup,
            Self::BackupStoreUnavailable => SubjectKind::BackupStore,
        }
    }

    /// VM에 걸리는 조건인지(`vms` 거르기를 받는다)
    fn about_vms(&self) -> bool {
        matches!(
            self,
            Self::VmMetric { .. } | Self::VmFailed | Self::HaRecovery
        )
    }

    /// 문턱(지표·풀 조건만)
    pub fn threshold(&self) -> Option<f64> {
        match self {
            Self::NodeMetric { value, .. } | Self::VmMetric { value, .. } => Some(*value),
            Self::PoolUsage { percent } => Some(*percent),
            _ => None,
        }
    }

    /// 노드 점 하나로 본 결과(`NodeMetric`만, 값을 모르면 `None`)
    pub fn observe_node(&self, s: &NodeSample) -> Option<Observation> {
        let Self::NodeMetric { metric, op, value } = self else {
            return None;
        };
        let v = metric.read(s)?;
        Some(Observation::new(op.holds(v, *value), Some(v)))
    }

    /// VM 점 하나로 본 결과(`VmMetric`만, 값을 모르면 `None`)
    pub fn observe_vm(&self, s: &VmSample) -> Option<Observation> {
        let Self::VmMetric { metric, op, value } = self else {
            return None;
        };
        let v = metric.read(s)?;
        Some(Observation::new(op.holds(v, *value), Some(v)))
    }

    /// 풀 용량으로 본 결과(`PoolUsage`만, 전체가 0이면 `None`)
    pub fn observe_pool(&self, total_bytes: u64, available_bytes: u64) -> Option<Observation> {
        let Self::PoolUsage { percent } = self else {
            return None;
        };
        if total_bytes == 0 {
            return None;
        }
        let used = total_bytes.saturating_sub(available_bytes) as f64 * 100.0 / total_bytes as f64;
        Some(Observation::new(used > *percent, Some(used)))
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum AlertRuleError {
    #[error("give 1-{MAX_RULE_TARGETS} targets, not {0}")]
    Targets(usize),
    #[error("target {0:?} is given twice")]
    DuplicateTarget(String),
    #[error("invalid target: {0}")]
    TargetName(NameError),
    #[error("for_secs {0} is out of range: use 0-{MAX_FOR_SECS}")]
    ForSecs(u32),
    #[error("{0} is an event condition: for_secs must be 0")]
    EventFor(&'static str),
    #[error(
        "repeat_secs {0} is out of range: use 0 (never) or {MIN_REPEAT_SECS}-{MAX_REPEAT_SECS}"
    )]
    Repeat(u32),
    #[error("the threshold must be a finite number")]
    Value,
    #[error("percent {0} is out of range: use 0-100")]
    Percent(f64),
    #[error("vms only works with vm conditions, not {0}")]
    VmsFilter(&'static str),
    #[error("nodes does not work with {0}")]
    NodesFilter(&'static str),
    #[error("give at most {MAX_RULE_SUBJECTS} names in nodes and vms")]
    TooManySubjects,
    #[error("invalid name in nodes or vms: {0}")]
    SubjectName(NameError),
    #[error("give at most {MAX_RULE_OPS} task ops")]
    TooManyOps,
    #[error("invalid task op {0:?}: use a task kind like vm-start")]
    Op(String),
}

impl AlertRuleSpec {
    /// 형식만 본다(대상·노드·VM이 있는지는 API가 본다).
    pub fn validate(&self) -> Result<(), AlertRuleError> {
        if self.targets.is_empty() || self.targets.len() > MAX_RULE_TARGETS {
            return Err(AlertRuleError::Targets(self.targets.len()));
        }
        let mut seen = BTreeSet::new();
        for t in &self.targets {
            validate_name(t).map_err(AlertRuleError::TargetName)?;
            if !seen.insert(t) {
                return Err(AlertRuleError::DuplicateTarget(t.clone()));
            }
        }
        if self.for_secs > MAX_FOR_SECS {
            return Err(AlertRuleError::ForSecs(self.for_secs));
        }
        let c = &self.condition;
        if c.is_event() && self.for_secs != 0 {
            return Err(AlertRuleError::EventFor(c.kind()));
        }
        if self.repeat_secs != 0 && !(MIN_REPEAT_SECS..=MAX_REPEAT_SECS).contains(&self.repeat_secs)
        {
            return Err(AlertRuleError::Repeat(self.repeat_secs));
        }
        match c {
            AlertCondition::NodeMetric { value, .. } | AlertCondition::VmMetric { value, .. }
                if !value.is_finite() =>
            {
                return Err(AlertRuleError::Value);
            }
            AlertCondition::PoolUsage { percent } if !(0.0..=100.0).contains(percent) => {
                return Err(AlertRuleError::Percent(*percent));
            }
            AlertCondition::TaskFailed { ops } => {
                if ops.len() > MAX_RULE_OPS {
                    return Err(AlertRuleError::TooManyOps);
                }
                if let Some(op) = ops.iter().find(|op| validate_name(op).is_err()) {
                    return Err(AlertRuleError::Op(op.clone()));
                }
            }
            _ => {}
        }
        if !self.vms.is_empty() && !c.about_vms() {
            return Err(AlertRuleError::VmsFilter(c.kind()));
        }
        // 노드 거르기는 노드에서 일어나는 일에만 뜻이 있다(SDN 조건은 노드가 대상인 것만 — 계획 67 §11).
        if !self.nodes.is_empty()
            && (matches!(c, AlertCondition::BackupJobFailed)
                || c.is_backup_store()
                || (c.is_sdn() && c.subject_kind() != SubjectKind::Node))
        {
            return Err(AlertRuleError::NodesFilter(c.kind()));
        }
        if self.nodes.len() > MAX_RULE_SUBJECTS || self.vms.len() > MAX_RULE_SUBJECTS {
            return Err(AlertRuleError::TooManySubjects);
        }
        for n in self.nodes.iter().chain(&self.vms) {
            validate_name(n).map_err(AlertRuleError::SubjectName)?;
        }
        Ok(())
    }

    /// 노드 `name`에 걸리는지(`nodes`가 비면 모두)
    pub fn applies_to_node(&self, name: &str) -> bool {
        self.nodes.is_empty() || self.nodes.iter().any(|n| n == name)
    }

    /// VM `name`에 걸리는지(`vms`가 비면 모두)
    pub fn applies_to_vm(&self, name: &str) -> bool {
        self.vms.is_empty() || self.vms.iter().any(|n| n == name)
    }

    /// 이 Task 종류(`TaskOp`의 `kind`)가 `TaskFailed` 규칙에 걸리는지
    pub fn applies_to_op(&self, op: &str) -> bool {
        match &self.condition {
            AlertCondition::TaskFailed { ops } => ops.is_empty() || ops.iter().any(|o| o == op),
            _ => false,
        }
    }
}

// ---- 활성 경보와 상태 기계 ----

/// 경보 대상의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    Node,
    Vm,
    Pool,
    Task,
    BackupJob,
    /// 클러스터 SDN(`Sdn` 리소스, 계획 67)
    Sdn,
    /// SDN 라우터(계획 67)
    Router,
    /// 부하 분산기(계획 67)
    LoadBalancer,
    /// 백업(계획 68)
    Backup,
    /// 백업 저장소(계획 68)
    BackupStore,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Vm => "vm",
            Self::Pool => "pool",
            Self::Task => "task",
            Self::BackupJob => "backup_job",
            Self::Sdn => "sdn",
            Self::Router => "router",
            Self::LoadBalancer => "load_balancer",
            Self::Backup => "backup",
            Self::BackupStore => "backup_store",
        }
    }
}

/// 경보가 가리키는 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertSubject {
    pub kind: SubjectKind,
    pub id: Uuid,
    /// 알릴 때의 이름
    pub name: String,
    /// 그 일이 일어난 노드 이름(노드·풀 경보는 평가한 노드)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub node: Option<String>,
}

impl AlertSubject {
    /// 경보 키: 규칙 id + 대상 종류·id
    pub fn key(&self, rule: Uuid) -> String {
        format!("{rule}/{}/{}", self.kind.as_str(), self.id)
    }
}

/// 경보의 단계
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    /// 조건이 참이지만 `for_secs`가 아직 지나지 않음
    Pending,
    /// 알림
    Firing,
}

/// 활성 경보 하나
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ActiveAlert {
    pub rule: Uuid,
    pub subject: AlertSubject,
    pub state: AlertState,
    /// 조건이 참이 된 때
    pub since: i64,
    /// 마지막으로 본 값(지표·풀 조건)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub value: Option<f64>,
    /// 마지막으로 알린 때
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_sent: Option<i64>,
    /// `firing`인데 조건이 거짓이 된 때(이만큼 [`RESOLVE_AFTER_MS`] 지나면 해제)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub clear_since: Option<i64>,
}

impl ActiveAlert {
    /// [`AlertSubject::key`]
    pub fn key(&self) -> String {
        self.subject.key(self.rule)
    }
}

/// 한 번 본 조건
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    /// 조건이 참인지
    pub holds: bool,
    /// 본 값(지표·풀 조건)
    pub value: Option<f64>,
}

impl Observation {
    pub fn new(holds: bool, value: Option<f64>) -> Self {
        Self { holds, value }
    }
}

/// 보낼 알림
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// 처음 `firing`
    Fire,
    /// `firing`인 채 `repeat_secs`가 지남
    Repeat,
    /// 해제(`send_resolved`일 때만)
    Resolve,
}

/// 상태(지표·상태 조건) 경보 하나를 한 걸음 진행한다. `seen`이 `None`(값을 모름)이면 단계를 바꾸지 않고 반복만
/// 본다. 돌려준 경보가 `None`이면 활성 경보에서 뺀다. 대상이 사라지면 이 함수를 부르지 않고 알림 없이 뺀다.
/// `resolve_after_ms`는 보통 [`RESOLVE_AFTER_MS`]다(시험이 줄인다).
pub fn advance(
    rule_id: Uuid,
    rule: &AlertRuleSpec,
    subject: &AlertSubject,
    current: Option<ActiveAlert>,
    seen: Option<Observation>,
    now: i64,
    resolve_after_ms: i64,
) -> (Option<ActiveAlert>, Option<Notice>) {
    let for_ms = i64::from(rule.for_secs) * 1000;
    let repeat_due = |a: &ActiveAlert| {
        rule.repeat_secs > 0
            && a.last_sent
                .is_some_and(|t| now - t >= i64::from(rule.repeat_secs) * 1000)
    };
    let Some(seen) = seen else {
        return match current {
            Some(mut a) if a.state == AlertState::Firing && repeat_due(&a) => {
                a.last_sent = Some(now);
                (Some(a), Some(Notice::Repeat))
            }
            other => (other, None),
        };
    };
    match (current, seen.holds) {
        (None, false) => (None, None),
        (None, true) => {
            let mut a = ActiveAlert {
                rule: rule_id,
                subject: subject.clone(),
                state: AlertState::Pending,
                since: now,
                value: seen.value,
                last_sent: None,
                clear_since: None,
            };
            if for_ms == 0 {
                a.state = AlertState::Firing;
                a.last_sent = Some(now);
                return (Some(a), Some(Notice::Fire));
            }
            (Some(a), None)
        }
        (Some(mut a), true) => {
            a.value = seen.value;
            a.clear_since = None;
            match a.state {
                AlertState::Pending if now - a.since >= for_ms => {
                    a.state = AlertState::Firing;
                    a.last_sent = Some(now);
                    (Some(a), Some(Notice::Fire))
                }
                AlertState::Pending => (Some(a), None),
                AlertState::Firing if repeat_due(&a) => {
                    a.last_sent = Some(now);
                    (Some(a), Some(Notice::Repeat))
                }
                AlertState::Firing => (Some(a), None),
            }
        }
        (Some(a), false) if a.state == AlertState::Pending => (None, None),
        (Some(mut a), false) => {
            a.value = seen.value;
            match a.clear_since {
                Some(t) if now - t >= resolve_after_ms => {
                    (None, rule.send_resolved.then_some(Notice::Resolve))
                }
                Some(_) => (Some(a), None),
                None => {
                    a.clear_since = Some(now);
                    (Some(a), None)
                }
            }
        }
    }
}

// ---- 알림 대상 ----

/// 알림 대상 하나. admin만 읽고 쓰며, API 응답에서 비밀을 뺀다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct NotificationTargetSpec {
    pub target: TargetConfig,
    /// 끄면 보내지 않는다(시험 보내기는 된다).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub enabled: bool,
}

impl ResourceSpec for NotificationTargetSpec {
    const KIND: Kind = Kind::NotificationTarget;
    type Status = NotificationTargetStatus;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct NotificationTargetStatus {}

/// 보내는 방법
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetConfig {
    Smtp(SmtpTarget),
    Webhook(WebhookTarget),
}

impl TargetConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Smtp(_) => "smtp",
            Self::Webhook(_) => "webhook",
        }
    }
}

/// SMTP 연결 보안
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SmtpTls {
    /// 평문으로 붙어 STARTTLS(보통 587)
    #[default]
    Starttls,
    /// 처음부터 TLS(보통 465)
    Tls,
    /// 암호화 없음(127.0.0.1 중계용)
    None,
}

/// 메일(일반 텍스트 본문)
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct SmtpTarget {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub tls: SmtpTls,
    /// AUTH 사용자. 없으면 인증하지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub user: Option<String>,
    /// AUTH 비밀번호. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub password: Option<String>,
    /// 보내는 주소
    pub from: String,
    /// 받는 주소(1~20개)
    pub to: Vec<String>,
}

// 비밀번호가 로그에 나가지 않게 가린다.
impl std::fmt::Debug for SmtpTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("from", &self.from)
            .field("to", &self.to)
            .finish()
    }
}

/// webhook 본문 형식
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum WebhookFormat {
    /// `{version: 1, cluster, state, severity, rule, subject, value, threshold, since, at}`
    #[default]
    Json,
    /// `{"text": …}`(Mattermost·Rocket.Chat과도 맞음)
    Slack,
    /// Teams Workflows Adaptive Card
    Teams,
    /// 텍스트 본문 + `Title`·`Priority`·`Tags` 헤더, `token`이면 `Authorization: Bearer`
    Ntfy,
    /// `{title, message, priority}`, `token`은 `X-Gotify-Key`
    Gotify,
}

impl WebhookFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Slack => "slack",
            Self::Teams => "teams",
            Self::Ntfy => "ntfy",
            Self::Gotify => "gotify",
        }
    }

    /// `token`을 쓰는 형식인지
    pub fn uses_token(self) -> bool {
        matches!(self, Self::Ntfy | Self::Gotify)
    }
}

/// HTTP(S) POST
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct WebhookTarget {
    /// `http(s)://…`. 경로·질의는 비밀로 본다(Slack·Teams URL). API는 [`redact_url`]로 줄여 보인다.
    pub url: String,
    #[serde(default)]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub format: WebhookFormat,
    /// 더 붙일 헤더. 값은 비밀로 본다(API는 이름만 보인다).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub headers: BTreeMap<String, String>,
    /// ntfy·Gotify 토큰. API는 내보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub token: Option<String>,
    /// 있으면 이 CA(PEM)만 믿는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
}

// URL의 경로·질의, 헤더 값, 토큰이 로그에 나가지 않게 가리고, 긴 CA PEM은 뺀다.
impl std::fmt::Debug for WebhookTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let headers: BTreeMap<&str, &str> =
            self.headers.keys().map(|k| (k.as_str(), "***")).collect();
        f.debug_struct("WebhookTarget")
            .field("url", &redact_url(&self.url))
            .field("format", &self.format)
            .field("headers", &headers)
            .field("token", &self.token.as_ref().map(|_| "***"))
            .finish_non_exhaustive()
    }
}

/// URL을 `scheme://host[:port]/…`로 줄인다(경로·질의·사용자 정보를 뺀다). 경로가 `/`뿐이고 질의가 없으면
/// `scheme://host[:port]/`이다. 풀지 못하면 `<invalid url>`이다.
pub fn redact_url(url: &str) -> String {
    let Ok(u) = url::Url::parse(url.trim()) else {
        return "<invalid url>".into();
    };
    let Some(host) = u.host_str() else {
        return "<invalid url>".into();
    };
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    let tail = if u.path() == "/" && u.query().is_none() && u.fragment().is_none() {
        "/"
    } else {
        "/…"
    };
    format!("{}://{host}{port}{tail}", u.scheme())
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum TargetError {
    #[error("invalid SMTP host {0:?}")]
    Host(String),
    #[error("the SMTP port must not be 0")]
    Port,
    #[error("a password needs a user")]
    PasswordWithoutUser,
    #[error("invalid SMTP user")]
    User,
    #[error("invalid mail address {0:?}")]
    Address(String),
    #[error("give 1-{MAX_SMTP_RECIPIENTS} recipients, not {0}")]
    Recipients(usize),
    #[error("invalid webhook url {0}: use http:// or https:// with a host")]
    Url(String),
    #[error("give at most {MAX_WEBHOOK_HEADERS} headers")]
    TooManyHeaders,
    #[error("invalid header {0:?}")]
    Header(String),
    #[error("an empty or invalid token")]
    Token,
    #[error("format {0} does not use a token; put it in headers")]
    TokenUnused(&'static str),
    #[error("ca_pem must hold a PEM certificate")]
    CaPem,
}

impl NotificationTargetSpec {
    /// 형식만 본다(닿는지는 시험 보내기가 본다). 오류 문구에 비밀을 넣지 않는다.
    pub fn validate(&self) -> Result<(), TargetError> {
        match &self.target {
            TargetConfig::Smtp(s) => s.validate(),
            TargetConfig::Webhook(w) => w.validate(),
        }
    }
}

fn has_control(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// `local@domain` 모양(공백·제어 문자·`<>,;"` 없음)인지만 본다.
fn valid_address(a: &str) -> bool {
    let Some((local, domain)) = a.rsplit_once('@') else {
        return false;
    };
    a.len() <= 254
        && !local.is_empty()
        && !domain.is_empty()
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !a
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>,;\"()[]\\".contains(c))
}

impl SmtpTarget {
    pub fn validate(&self) -> Result<(), TargetError> {
        let host = &self.host;
        if host.is_empty()
            || host.len() > 253
            || host.chars().any(|c| c.is_whitespace() || c.is_control())
            || host.contains(['/', '@'])
        {
            return Err(TargetError::Host(host.clone()));
        }
        if self.port == 0 {
            return Err(TargetError::Port);
        }
        if self.password.is_some() && self.user.is_none() {
            return Err(TargetError::PasswordWithoutUser);
        }
        if let Some(user) = &self.user
            && (user.is_empty() || has_control(user))
        {
            return Err(TargetError::User);
        }
        if !valid_address(&self.from) {
            return Err(TargetError::Address(self.from.clone()));
        }
        if self.to.is_empty() || self.to.len() > MAX_SMTP_RECIPIENTS {
            return Err(TargetError::Recipients(self.to.len()));
        }
        if let Some(a) = self.to.iter().find(|a| !valid_address(a)) {
            return Err(TargetError::Address(a.clone()));
        }
        Ok(())
    }
}

/// HTTP 헤더 이름(토큰 문자)인지
fn valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// 보내는 쪽이 정하는 헤더(바꾸지 못함)
const RESERVED_HEADERS: [&str; 4] = ["host", "content-length", "transfer-encoding", "connection"];

impl WebhookTarget {
    pub fn validate(&self) -> Result<(), TargetError> {
        let ok = url::Url::parse(self.url.trim()).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| !h.is_empty())
        });
        if !ok {
            return Err(TargetError::Url(redact_url(&self.url)));
        }
        if self.headers.len() > MAX_WEBHOOK_HEADERS {
            return Err(TargetError::TooManyHeaders);
        }
        for (name, value) in &self.headers {
            if !valid_header_name(name)
                || RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str())
                || has_control(value)
            {
                return Err(TargetError::Header(name.clone()));
            }
        }
        if let Some(token) = &self.token {
            if token.is_empty() || has_control(token) {
                return Err(TargetError::Token);
            }
            if !self.format.uses_token() {
                return Err(TargetError::TokenUnused(self.format.as_str()));
            }
        }
        if let Some(pem) = &self.ca_pem
            && !pem.contains("-----BEGIN CERTIFICATE-----")
        {
            return Err(TargetError::CaPem);
        }
        Ok(())
    }
}

// ---- 보낸 기록 ----

/// 알림의 종류
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum NotificationState {
    /// 발생(반복 포함)
    Firing,
    Resolved,
    /// 이벤트 조건
    Event,
    /// 시험 보내기
    Test,
}

/// 보낸 알림 하나(보낸 노드의 `alerts/history.log` 한 줄)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationRecord {
    /// 보낸(또는 마지막으로 해 본) 때
    pub at: i64,
    /// 보낸 노드 이름
    pub node: String,
    /// 규칙 이름(시험 보내기는 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub rule: Option<String>,
    /// 대상 이름(예: VM 이름, 시험 보내기는 없음)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub subject: Option<String>,
    pub state: NotificationState,
    /// 알림 대상 이름. admin이 아닌 사용자에게는 보이지 않는다(빈 이름이면 빼고 보낸다).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub target: String,
    pub ok: bool,
    /// 실패 이유. 보낸 쪽이 비밀을 지운 문구다(대기열이 넘쳐 버렸으면 `dropped`로 시작). admin이 아닌 사용자에게는
    /// 보이지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

// ---- 노드 사이 읽기(`ReadAlerts`)와 API 응답 ----

/// `GET /alerts/history`의 기본·최대 개수
pub const DEFAULT_HISTORY_LIMIT: u32 = 200;
pub const MAX_HISTORY_LIMIT: u32 = 5000;

/// 보낸 기록을 읽는 창(시각은 ms, 둘 다 포함)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryWindow {
    pub since: i64,
    pub until: i64,
    /// 새 것부터 이만큼(+1, 잘렸는지 알려고)
    pub limit: u32,
}

/// 한 노드의 경보 읽기(peer RPC `ReadAlerts`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AlertsQuery {
    /// 있으면 보낸 기록도 읽는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HistoryWindow>,
}

/// 한 노드가 가진 노드 경보와 보낸 기록
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AlertsReply {
    /// 그 노드가 평가하는 활성 경보(`pending` 포함)
    #[serde(default)]
    pub active: Vec<ActiveAlert>,
    /// 보낸 기록(새 것부터, 최대 `limit + 1`개)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<NotificationRecord>,
    /// 활성 경보가 상한(노드마다 500개)을 넘어 더 생긴 경보를 따라가지 못하고 있다.
    #[serde(default, skip_serializing_if = "is_false")]
    pub overflow: bool,
}

/// `GET /alerts`의 경보 하나(규칙 이름·심각도를 붙인 [`ActiveAlert`])
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertView {
    pub rule: Uuid,
    pub rule_name: String,
    pub severity: Severity,
    /// 조건 종류(`node_metric`, `node_offline` …)
    pub condition: String,
    pub subject: AlertSubject,
    pub state: AlertState,
    pub since: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub value: Option<f64>,
    /// 문턱(지표·풀 조건)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub last_sent: Option<i64>,
    /// 해제를 기다리기 시작한 때(조건이 거짓이 됨)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub clear_since: Option<i64>,
}

/// `GET /alerts`의 응답
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct AlertsResponse {
    /// `firing` 먼저, 심각도 높은 것 먼저, 오래된 것 먼저
    pub alerts: Vec<AlertView>,
    /// 답하지 않은 노드 이름(그 노드의 노드 경보는 보이지 않는다)
    pub unreachable: Vec<String>,
    /// 경보를 모르는 옛 노드 이름
    pub unsupported: Vec<String>,
    /// 활성 경보가 상한을 넘은 노드 이름
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub overflowing: Vec<String>,
}

/// `GET /alerts/history`의 응답
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationHistoryResponse {
    /// 새 것부터
    pub records: Vec<NotificationRecord>,
    pub unreachable: Vec<String>,
    pub unsupported: Vec<String>,
    /// `limit`보다 많아 잘랐다.
    pub truncated: bool,
}

/// 시험 보내기의 결과
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationTestResult {
    pub ok: bool,
    /// 실패 이유(비밀을 지운 문구)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub error: Option<String>,
}

/// 응답의 알림 대상. 비밀(비밀번호·토큰·헤더 값·URL의 경로와 질의) 대신 있는지만 보인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct NotificationTargetView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub enabled: bool,
    pub target: TargetConfigView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TargetConfigView {
    Smtp(SmtpTargetView),
    Webhook(WebhookTargetView),
}

/// [`SmtpTarget`]에서 `password`를 뺀 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct SmtpTargetView {
    pub host: String,
    pub port: u16,
    pub tls: SmtpTls,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub user: Option<String>,
    pub has_password: bool,
    pub from: String,
    pub to: Vec<String>,
}

/// [`WebhookTarget`]에서 비밀을 뺀 것
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct WebhookTargetView {
    /// [`redact_url`]로 줄인 URL(`scheme://host[:port]/…`). 바꾸기 본문에 이 값을 그대로 주면 저장된 URL을 지킨다.
    pub url: String,
    pub format: WebhookFormat,
    /// 헤더 이름(값은 비밀)
    pub header_names: Vec<String>,
    pub has_token: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub ca_pem: Option<String>,
}

impl From<TargetConfig> for TargetConfigView {
    fn from(t: TargetConfig) -> Self {
        match t {
            TargetConfig::Smtp(s) => Self::Smtp(SmtpTargetView {
                host: s.host,
                port: s.port,
                tls: s.tls,
                user: s.user,
                has_password: s.password.is_some(),
                from: s.from,
                to: s.to,
            }),
            TargetConfig::Webhook(w) => Self::Webhook(WebhookTargetView {
                url: redact_url(&w.url),
                format: w.format,
                header_names: w.headers.into_keys().collect(),
                has_token: w.token.is_some(),
                ca_pem: w.ca_pem,
            }),
        }
    }
}

impl From<NotificationTarget> for NotificationTargetView {
    fn from(t: NotificationTarget) -> Self {
        Self {
            id: t.id,
            name: t.name,
            revision: t.revision,
            enabled: t.spec.enabled,
            target: t.spec.target.into(),
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn default_repeat_secs() -> u32 {
    DEFAULT_REPEAT_SECS
}

fn is_default_repeat(n: &u32) -> bool {
    *n == DEFAULT_REPEAT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(condition: AlertCondition) -> AlertRuleSpec {
        AlertRuleSpec {
            enabled: true,
            condition,
            for_secs: 0,
            severity: Severity::Warning,
            targets: vec!["ops-mail".into()],
            repeat_secs: DEFAULT_REPEAT_SECS,
            send_resolved: true,
            nodes: vec![],
            vms: vec![],
        }
    }

    fn cpu_rule() -> AlertRuleSpec {
        rule(AlertCondition::NodeMetric {
            metric: NodeAlertMetric::Cpu,
            op: AlertOp::Gt,
            value: 90.0,
        })
    }

    #[test]
    fn kinds_are_named() {
        assert_eq!(AlertRuleSpec::KIND, Kind::AlertRule);
        assert_eq!(Kind::AlertRule.as_str(), "alert-rule");
        assert_eq!(serde_json::to_value(Kind::AlertRule).unwrap(), "alert-rule");
        assert_eq!(NotificationTargetSpec::KIND, Kind::NotificationTarget);
        assert_eq!(Kind::NotificationTarget.as_str(), "notification-target");
        assert_eq!(
            serde_json::to_value(Kind::NotificationTarget).unwrap(),
            "notification-target"
        );
    }

    /// 기본값은 쓰지 않는다.
    #[test]
    fn rule_json_is_minimal_and_round_trips() {
        let r = cpu_rule();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            json!({
                "condition": {"kind": "node_metric", "metric": "cpu", "op": "gt", "value": 90.0},
                "severity": "warning",
                "targets": ["ops-mail"]
            })
        );
        assert_eq!(serde_json::from_value::<AlertRuleSpec>(v).unwrap(), r);
        let full = AlertRuleSpec {
            enabled: false,
            for_secs: 300,
            repeat_secs: 0,
            send_resolved: false,
            nodes: vec!["n1".into()],
            vms: vec![],
            ..cpu_rule()
        };
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v["enabled"], false);
        assert_eq!(v["for_secs"], 300);
        assert_eq!(v["repeat_secs"], 0);
        assert_eq!(v["send_resolved"], false);
        assert_eq!(v["nodes"], json!(["n1"]));
        assert_eq!(serde_json::from_value::<AlertRuleSpec>(v).unwrap(), full);
    }

    #[test]
    fn rule_rejects_unknown_fields() {
        let bad = json!({
            "condition": {"kind": "vm_failed"},
            "severity": "critical",
            "targets": ["a"],
            "extra": 1
        });
        assert!(serde_json::from_value::<AlertRuleSpec>(bad).is_err());
        let bad = json!({
            "condition": {"kind": "pool_usage", "percent": 1.0, "x": 1},
            "severity": "critical",
            "targets": ["a"]
        });
        assert!(serde_json::from_value::<AlertRuleSpec>(bad).is_err());
        let bad = json!({"condition": {"kind": "nope"}, "severity": "info", "targets": ["a"]});
        assert!(serde_json::from_value::<AlertRuleSpec>(bad).is_err());
    }

    #[test]
    fn conditions_serialize_by_kind() {
        let cases = [
            (
                AlertCondition::VmMetric {
                    metric: VmAlertMetric::DiskReadIops,
                    op: AlertOp::Lt,
                    value: 1.5,
                },
                json!({"kind": "vm_metric", "metric": "disk_read_iops", "op": "lt", "value": 1.5}),
            ),
            (
                AlertCondition::PoolUsage { percent: 85.0 },
                json!({"kind": "pool_usage", "percent": 85.0}),
            ),
            (AlertCondition::NodeOffline, json!({"kind": "node_offline"})),
            (AlertCondition::VmFailed, json!({"kind": "vm_failed"})),
            (
                AlertCondition::TaskFailed { ops: vec![] },
                json!({"kind": "task_failed"}),
            ),
            (
                AlertCondition::TaskFailed {
                    ops: vec!["vm-start".into()],
                },
                json!({"kind": "task_failed", "ops": ["vm-start"]}),
            ),
            (AlertCondition::HaRecovery, json!({"kind": "ha_recovery"})),
            (
                AlertCondition::BackupJobFailed,
                json!({"kind": "backup_job_failed"}),
            ),
        ];
        for (c, want) in cases {
            assert_eq!(serde_json::to_value(&c).unwrap(), want);
            assert_eq!(want["kind"], c.kind());
            assert_eq!(serde_json::from_value::<AlertCondition>(want).unwrap(), c);
        }
    }

    /// 계획 67: SDN 조건 여덟은 리더가 평가하고, 대상 종류와 노드 거르기가 맞고, 옛 규칙 JSON은 그대로다.
    #[test]
    fn sdn_conditions() {
        let all = [
            (
                AlertCondition::SdnQuorumLost,
                "sdn_quorum_lost",
                SubjectKind::Sdn,
            ),
            (
                AlertCondition::SdnCentralDown,
                "sdn_central_down",
                SubjectKind::Node,
            ),
            (
                AlertCondition::SdnChassisDown,
                "sdn_chassis_down",
                SubjectKind::Node,
            ),
            (
                AlertCondition::SdnGatewayUnavailable,
                "sdn_gateway_unavailable",
                SubjectKind::Router,
            ),
            (
                AlertCondition::SdnGatewayFailover,
                "sdn_gateway_failover",
                SubjectKind::Router,
            ),
            (
                AlertCondition::SdnSyncFailed,
                "sdn_sync_failed",
                SubjectKind::Sdn,
            ),
            (
                AlertCondition::SdnPhysnetNotReady,
                "sdn_physnet_not_ready",
                SubjectKind::Node,
            ),
            (
                AlertCondition::SdnLbMemberDown,
                "sdn_lb_member_down",
                SubjectKind::LoadBalancer,
            ),
        ];
        for (c, kind, subject) in all {
            let json = json!({ "kind": kind });
            assert_eq!(serde_json::to_value(&c).unwrap(), json);
            assert_eq!(serde_json::from_value::<AlertCondition>(json).unwrap(), c);
            assert_eq!(c.kind(), kind);
            assert!(c.is_sdn());
            assert_eq!(c.evaluator(), Evaluator::Leader);
            assert_eq!(c.subject_kind(), subject);
            assert_eq!(c.is_event(), c == AlertCondition::SdnGatewayFailover);
            let mut r = rule(c.clone());
            assert_eq!(r.validate(), Ok(()));
            r.nodes = vec!["n1".into()];
            if subject == SubjectKind::Node {
                assert_eq!(r.validate(), Ok(()));
            } else {
                assert_eq!(r.validate(), Err(AlertRuleError::NodesFilter(kind)));
            }
            r.nodes.clear();
            r.vms = vec!["web".into()];
            assert_eq!(r.validate(), Err(AlertRuleError::VmsFilter(kind)));
        }
        assert!(!AlertCondition::NodeOffline.is_sdn());
        assert!(!cpu_rule().condition.is_sdn());
        for (k, s) in [
            (SubjectKind::Sdn, "sdn"),
            (SubjectKind::Router, "router"),
            (SubjectKind::LoadBalancer, "load_balancer"),
        ] {
            assert_eq!(k.as_str(), s);
            assert_eq!(serde_json::to_value(k).unwrap(), s);
            assert_eq!(serde_json::from_value::<SubjectKind>(json!(s)).unwrap(), k);
        }
        // 옛 규칙 JSON은 바이트까지 같다.
        let old = r#"{"condition":{"kind":"node_offline"},"for_secs":60,"severity":"critical","targets":["ops"],"nodes":["n1"]}"#;
        let r: AlertRuleSpec = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), old);
    }

    /// 계획 68: 백업 조건 둘은 리더가 평가하고(이벤트가 아님), 대상은 백업·저장소이며 노드·VM 거르기를 받지 않는다.
    #[test]
    fn backup_store_conditions() {
        for (c, kind, subject) in [
            (
                AlertCondition::BackupVerifyFailed,
                "backup_verify_failed",
                SubjectKind::Backup,
            ),
            (
                AlertCondition::BackupStoreUnavailable,
                "backup_store_unavailable",
                SubjectKind::BackupStore,
            ),
        ] {
            let json = json!({ "kind": kind });
            assert_eq!(serde_json::to_value(&c).unwrap(), json);
            assert_eq!(serde_json::from_value::<AlertCondition>(json).unwrap(), c);
            assert_eq!(c.kind(), kind);
            assert!(c.is_backup_store());
            assert!(!c.is_sdn());
            assert!(!c.is_event());
            assert_eq!(c.evaluator(), Evaluator::Leader);
            assert_eq!(c.subject_kind(), subject);
            let mut r = rule(c.clone());
            r.for_secs = 600;
            assert_eq!(r.validate(), Ok(()));
            r.nodes = vec!["n1".into()];
            assert_eq!(r.validate(), Err(AlertRuleError::NodesFilter(kind)));
            r.nodes.clear();
            r.vms = vec!["web".into()];
            assert_eq!(r.validate(), Err(AlertRuleError::VmsFilter(kind)));
        }
        assert!(!AlertCondition::BackupJobFailed.is_backup_store());
        for (k, s) in [
            (SubjectKind::Backup, "backup"),
            (SubjectKind::BackupStore, "backup_store"),
        ] {
            assert_eq!(k.as_str(), s);
            assert_eq!(serde_json::to_value(k).unwrap(), s);
            assert_eq!(serde_json::from_value::<SubjectKind>(json!(s)).unwrap(), k);
        }
    }

    #[test]
    fn condition_classes() {
        assert!(AlertCondition::HaRecovery.is_event());
        assert!(AlertCondition::BackupJobFailed.is_event());
        assert!(AlertCondition::TaskFailed { ops: vec![] }.is_event());
        assert!(!AlertCondition::VmFailed.is_event());
        assert!(!cpu_rule().condition.is_event());
        assert_eq!(AlertCondition::NodeOffline.evaluator(), Evaluator::Leader);
        assert_eq!(
            AlertCondition::BackupJobFailed.evaluator(),
            Evaluator::Leader
        );
        assert_eq!(AlertCondition::VmFailed.evaluator(), Evaluator::Node);
        assert_eq!(
            AlertCondition::PoolUsage { percent: 1.0 }.evaluator(),
            Evaluator::Node
        );
        assert_eq!(AlertCondition::HaRecovery.subject_kind(), SubjectKind::Task);
        assert_eq!(cpu_rule().condition.threshold(), Some(90.0));
        assert_eq!(AlertCondition::VmFailed.threshold(), None);
    }

    #[test]
    fn validates_rules() {
        assert_eq!(cpu_rule().validate(), Ok(()));
        let r = |f: fn(&mut AlertRuleSpec)| {
            let mut r = cpu_rule();
            f(&mut r);
            r.validate()
        };
        assert_eq!(r(|r| r.targets.clear()), Err(AlertRuleError::Targets(0)));
        assert_eq!(
            r(|r| r.targets = (0..17).map(|i| format!("t{i}")).collect()),
            Err(AlertRuleError::Targets(17))
        );
        assert_eq!(
            r(|r| r.targets = vec!["a".into(), "a".into()]),
            Err(AlertRuleError::DuplicateTarget("a".into()))
        );
        assert!(matches!(
            r(|r| r.targets = vec!["Bad".into()]),
            Err(AlertRuleError::TargetName(_))
        ));
        assert_eq!(r(|r| r.for_secs = 86_400), Ok(()));
        assert_eq!(
            r(|r| r.for_secs = 86_401),
            Err(AlertRuleError::ForSecs(86_401))
        );
        assert_eq!(r(|r| r.repeat_secs = 0), Ok(()));
        assert_eq!(r(|r| r.repeat_secs = 59), Err(AlertRuleError::Repeat(59)));
        assert_eq!(
            r(|r| r.repeat_secs = MAX_REPEAT_SECS + 1),
            Err(AlertRuleError::Repeat(MAX_REPEAT_SECS + 1))
        );
        assert_eq!(
            r(|r| r.condition = AlertCondition::NodeMetric {
                metric: NodeAlertMetric::Cpu,
                op: AlertOp::Gt,
                value: f64::NAN
            }),
            Err(AlertRuleError::Value)
        );
        assert_eq!(
            r(|r| r.condition = AlertCondition::VmMetric {
                metric: VmAlertMetric::Cpu,
                op: AlertOp::Gt,
                value: f64::INFINITY
            }),
            Err(AlertRuleError::Value)
        );
        assert_eq!(
            r(|r| r.condition = AlertCondition::PoolUsage { percent: 101.0 }),
            Err(AlertRuleError::Percent(101.0))
        );
        assert_eq!(
            r(|r| {
                r.condition = AlertCondition::HaRecovery;
                r.for_secs = 10;
            }),
            Err(AlertRuleError::EventFor("ha_recovery"))
        );
        assert_eq!(
            r(|r| r.vms = vec!["web".into()]),
            Err(AlertRuleError::VmsFilter("node_metric"))
        );
        assert_eq!(
            r(|r| {
                r.condition = AlertCondition::VmFailed;
                r.vms = vec!["web".into()];
            }),
            Ok(())
        );
        assert_eq!(
            r(|r| {
                r.condition = AlertCondition::BackupJobFailed;
                r.nodes = vec!["n1".into()];
            }),
            Err(AlertRuleError::NodesFilter("backup_job_failed"))
        );
        assert_eq!(
            r(|r| r.nodes = (0..257).map(|i| format!("n{i}")).collect()),
            Err(AlertRuleError::TooManySubjects)
        );
        assert!(matches!(
            r(|r| r.nodes = vec!["N 1".into()]),
            Err(AlertRuleError::SubjectName(_))
        ));
        assert_eq!(
            r(|r| r.condition = AlertCondition::TaskFailed {
                ops: vec!["vm start".into()]
            }),
            Err(AlertRuleError::Op("vm start".into()))
        );
    }

    #[test]
    fn filters() {
        let mut r = rule(AlertCondition::TaskFailed {
            ops: vec!["vm-start".into()],
        });
        assert!(r.applies_to_node("any"));
        assert!(r.applies_to_vm("any"));
        assert!(r.applies_to_op("vm-start"));
        assert!(!r.applies_to_op("vm-stop"));
        r.nodes = vec!["n1".into()];
        assert!(r.applies_to_node("n1") && !r.applies_to_node("n2"));
        r.condition = AlertCondition::TaskFailed { ops: vec![] };
        assert!(r.applies_to_op("vm-stop"));
        assert!(!cpu_rule().applies_to_op("vm-stop"));
    }

    fn node_sample() -> NodeSample {
        NodeSample {
            t: 0,
            cpu: Some(95.0),
            mem_used_mib: Some(750),
            mem_total_mib: Some(1000),
            disk_read_bps: None,
            disk_write_bps: Some(f64::NAN),
            net_rx_bps: Some(10.0),
            net_tx_bps: None,
            fs_used_mib: Some(1),
            fs_total_mib: Some(0),
        }
    }

    #[test]
    fn reads_metrics_from_samples() {
        let s = node_sample();
        assert_eq!(NodeAlertMetric::Cpu.read(&s), Some(95.0));
        assert_eq!(NodeAlertMetric::MemPercent.read(&s), Some(75.0));
        assert_eq!(NodeAlertMetric::FsPercent.read(&s), None);
        assert_eq!(NodeAlertMetric::DiskRead.read(&s), None);
        assert_eq!(NodeAlertMetric::DiskWrite.read(&s), None);
        assert_eq!(
            cpu_rule().condition.observe_node(&s),
            Some(Observation::new(true, Some(95.0)))
        );
        let lt = AlertCondition::NodeMetric {
            metric: NodeAlertMetric::MemPercent,
            op: AlertOp::Lt,
            value: 50.0,
        };
        assert_eq!(
            lt.observe_node(&s),
            Some(Observation::new(false, Some(75.0)))
        );
        let none = AlertCondition::NodeMetric {
            metric: NodeAlertMetric::DiskRead,
            op: AlertOp::Gt,
            value: 0.0,
        };
        assert_eq!(none.observe_node(&s), None);
        assert_eq!(AlertCondition::VmFailed.observe_node(&s), None);
        let vm = VmSample {
            t: 0,
            cpu: None,
            mem_rss_mib: Some(2048),
            disk_read_bps: None,
            disk_write_bps: None,
            disk_read_iops: Some(3.0),
            disk_write_iops: None,
            net_rx_bps: None,
            net_tx_bps: None,
        };
        let rss = AlertCondition::VmMetric {
            metric: VmAlertMetric::MemRss,
            op: AlertOp::Gt,
            value: 1024.0,
        };
        assert_eq!(
            rss.observe_vm(&vm),
            Some(Observation::new(true, Some(2048.0)))
        );
        assert_eq!(VmAlertMetric::Cpu.read(&vm), None);
        let pool = AlertCondition::PoolUsage { percent: 80.0 };
        assert_eq!(
            pool.observe_pool(100, 10),
            Some(Observation::new(true, Some(90.0)))
        );
        assert_eq!(
            pool.observe_pool(100, 30),
            Some(Observation::new(false, Some(70.0)))
        );
        assert_eq!(pool.observe_pool(0, 0), None);
    }

    fn subject() -> AlertSubject {
        AlertSubject {
            kind: SubjectKind::Node,
            id: Uuid::from_u128(7),
            name: "n1".into(),
            node: Some("n1".into()),
        }
    }

    const RULE: Uuid = Uuid::from_u128(1);

    /// 상태 기계 표: (지금, 본 것) → 알림. 경보가 없어지면 `gone`.
    fn run(
        spec: &AlertRuleSpec,
        steps: &[(i64, Option<bool>)],
    ) -> Vec<(Option<Notice>, Option<AlertState>)> {
        let mut cur = None;
        let mut out = vec![];
        for &(now, seen) in steps {
            let seen = seen.map(|h| Observation::new(h, Some(1.0)));
            let (next, notice) = advance(RULE, spec, &subject(), cur, seen, now, RESOLVE_AFTER_MS);
            out.push((notice, next.as_ref().map(|a| a.state)));
            cur = next;
        }
        out
    }

    #[test]
    fn pending_firing_repeat_resolved() {
        let spec = AlertRuleSpec {
            for_secs: 10,
            repeat_secs: 60,
            ..cpu_rule()
        };
        use AlertState::*;
        let got = run(
            &spec,
            &[
                (0, Some(false)),
                (5_000, Some(true)),
                (10_000, Some(true)),
                (15_000, Some(true)),
                (20_000, None),
                (60_000, Some(true)),
                (75_000, Some(true)),
                (80_000, Some(false)),
                (100_000, Some(true)),
                (110_000, Some(false)),
                (130_000, None),
                (169_999, Some(false)),
                (170_000, Some(false)),
                (175_000, Some(false)),
            ],
        );
        assert_eq!(
            got,
            vec![
                (None, None),
                (None, Some(Pending)),
                (None, Some(Pending)),
                (Some(Notice::Fire), Some(Firing)),
                (None, Some(Firing)),
                (None, Some(Firing)),
                (Some(Notice::Repeat), Some(Firing)),
                (None, Some(Firing)),
                // 해제 대기 중 다시 참이면 처음부터
                (None, Some(Firing)),
                (None, Some(Firing)),
                // 값을 모르면 그대로(반복은 아직)
                (None, Some(Firing)),
                (None, Some(Firing)),
                (Some(Notice::Resolve), None),
                (None, None),
            ]
        );
    }

    #[test]
    fn pending_that_clears_is_dropped_silently() {
        let spec = AlertRuleSpec {
            for_secs: 60,
            ..cpu_rule()
        };
        let got = run(&spec, &[(0, Some(true)), (5_000, Some(false))]);
        assert_eq!(got, vec![(None, Some(AlertState::Pending)), (None, None)]);
    }

    #[test]
    fn for_zero_fires_at_once_and_repeat_works_while_unknown() {
        let spec = AlertRuleSpec {
            repeat_secs: 60,
            send_resolved: false,
            ..cpu_rule()
        };
        let got = run(
            &spec,
            &[
                (0, Some(true)),
                (60_000, None),
                (61_000, Some(false)),
                (121_000, Some(false)),
            ],
        );
        assert_eq!(
            got,
            vec![
                (Some(Notice::Fire), Some(AlertState::Firing)),
                (Some(Notice::Repeat), Some(AlertState::Firing)),
                (None, Some(AlertState::Firing)),
                // send_resolved가 꺼져 있으면 알림 없이 해제
                (None, None),
            ]
        );
        // repeat_secs = 0이면 다시 보내지 않는다
        let spec = AlertRuleSpec {
            repeat_secs: 0,
            ..cpu_rule()
        };
        let got = run(&spec, &[(0, Some(true)), (1_000_000_000, Some(true))]);
        assert_eq!(got[1].0, None);
    }

    #[test]
    fn active_alert_json_and_key() {
        let a = ActiveAlert {
            rule: RULE,
            subject: subject(),
            state: AlertState::Firing,
            since: 5,
            value: Some(91.5),
            last_sent: Some(6),
            clear_since: None,
        };
        assert_eq!(a.key(), format!("{RULE}/node/{}", Uuid::from_u128(7)));
        let v = serde_json::to_value(&a).unwrap();
        assert!(v.get("clear_since").is_none());
        assert_eq!(v["subject"]["kind"], "node");
        assert_eq!(v["state"], "firing");
        assert_eq!(serde_json::from_value::<ActiveAlert>(v).unwrap(), a);
        // 빈 상태는 `{}`
        assert_eq!(
            serde_json::to_value(AlertRuleStatus::default()).unwrap(),
            json!({})
        );
        let status = AlertRuleStatus {
            active: vec![a],
            ..Default::default()
        };
        let v = serde_json::to_value(&status).unwrap();
        assert_eq!(
            serde_json::from_value::<AlertRuleStatus>(v).unwrap(),
            status
        );
    }

    fn smtp() -> SmtpTarget {
        SmtpTarget {
            host: "mail.example.com".into(),
            port: 587,
            tls: SmtpTls::Starttls,
            user: Some("ops".into()),
            password: Some("hunter2-secret".into()),
            from: "orbivisor@example.com".into(),
            to: vec!["ops@example.com".into()],
        }
    }

    fn webhook() -> WebhookTarget {
        WebhookTarget {
            url: "https://hooks.slack.com/services/T000/B000/SECRETPATH?x=1".into(),
            format: WebhookFormat::Slack,
            headers: BTreeMap::from([("X-Api-Key".into(), "header-secret".into())]),
            token: None,
            ca_pem: None,
        }
    }

    #[test]
    fn target_json_round_trips() {
        let t = NotificationTargetSpec {
            target: TargetConfig::Smtp(smtp()),
            enabled: true,
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v,
            json!({"target": {
                "kind": "smtp", "host": "mail.example.com", "port": 587, "tls": "starttls",
                "user": "ops", "password": "hunter2-secret", "from": "orbivisor@example.com",
                "to": ["ops@example.com"]
            }})
        );
        assert_eq!(
            serde_json::from_value::<NotificationTargetSpec>(v).unwrap(),
            t
        );
        let w = NotificationTargetSpec {
            target: TargetConfig::Webhook(WebhookTarget {
                format: WebhookFormat::Json,
                headers: BTreeMap::new(),
                ..webhook()
            }),
            enabled: false,
        };
        let v = serde_json::to_value(&w).unwrap();
        assert_eq!(v["enabled"], false);
        assert!(v["target"].get("headers").is_none());
        assert_eq!(v["target"]["format"], "json");
        assert_eq!(
            serde_json::from_value::<NotificationTargetSpec>(v).unwrap(),
            w
        );
        // 기본 형식은 json, 기본 TLS는 starttls
        let w: WebhookTarget = serde_json::from_value(json!({"url": "http://h/"})).unwrap();
        assert_eq!(w.format, WebhookFormat::Json);
        let bad = json!({"target": {"kind": "webhook", "url": "http://h/", "x": 1}});
        assert!(serde_json::from_value::<NotificationTargetSpec>(bad).is_err());
        let bad = json!({"target": {"kind": "webhook", "url": "http://h/"}, "x": 1});
        assert!(serde_json::from_value::<NotificationTargetSpec>(bad).is_err());
        let bad = json!({"target": {"kind": "sms"}});
        assert!(serde_json::from_value::<NotificationTargetSpec>(bad).is_err());
    }

    /// 비밀은 `Debug`에 나오지 않는다.
    #[test]
    fn debug_hides_secrets() {
        let t = NotificationTargetSpec {
            target: TargetConfig::Smtp(smtp()),
            enabled: true,
        };
        let s = format!("{t:?}");
        assert!(!s.contains("hunter2"), "{s}");
        assert!(s.contains("mail.example.com"));
        let w = WebhookTarget {
            format: WebhookFormat::Ntfy,
            token: Some("tok-secret".into()),
            ca_pem: Some("-----BEGIN CERTIFICATE-----\nAAAA".into()),
            ..webhook()
        };
        let s = format!("{w:?}");
        for secret in ["SECRETPATH", "x=1", "header-secret", "tok-secret", "AAAA"] {
            assert!(!s.contains(secret), "{secret} in {s}");
        }
        assert!(s.contains("https://hooks.slack.com/…"), "{s}");
        assert!(s.contains("X-Api-Key"), "{s}");
    }

    #[test]
    fn redacts_urls() {
        assert_eq!(
            redact_url("https://hooks.slack.com/services/T/B/X"),
            "https://hooks.slack.com/…"
        );
        assert_eq!(
            redact_url("http://user:pw@10.0.0.1:8080/?q=1"),
            "http://10.0.0.1:8080/…"
        );
        assert_eq!(redact_url("https://ntfy.sh"), "https://ntfy.sh/");
        assert_eq!(redact_url("https://[::1]:9/"), "https://[::1]:9/");
        assert_eq!(redact_url("not a url"), "<invalid url>");
    }

    #[test]
    fn validates_smtp() {
        let t = |f: fn(&mut SmtpTarget)| {
            let mut s = smtp();
            f(&mut s);
            s.validate()
        };
        assert_eq!(t(|_| {}), Ok(()));
        assert_eq!(
            t(|s| {
                s.user = None;
                s.password = None;
                s.tls = SmtpTls::None;
            }),
            Ok(())
        );
        assert!(matches!(
            t(|s| s.host = "".into()),
            Err(TargetError::Host(_))
        ));
        assert!(matches!(
            t(|s| s.host = "a b".into()),
            Err(TargetError::Host(_))
        ));
        assert_eq!(t(|s| s.port = 0), Err(TargetError::Port));
        assert_eq!(t(|s| s.user = None), Err(TargetError::PasswordWithoutUser));
        assert_eq!(t(|s| s.user = Some("a\nb".into())), Err(TargetError::User));
        assert!(matches!(
            t(|s| s.from = "nobody".into()),
            Err(TargetError::Address(_))
        ));
        assert!(matches!(
            t(|s| s.to = vec!["a@b\r\nBcc: x@y".into()]),
            Err(TargetError::Address(_))
        ));
        assert_eq!(t(|s| s.to.clear()), Err(TargetError::Recipients(0)));
        assert_eq!(
            t(|s| s.to = (0..21).map(|i| format!("u{i}@example.com")).collect()),
            Err(TargetError::Recipients(21))
        );
    }

    #[test]
    fn validates_webhooks() {
        let t = |f: fn(&mut WebhookTarget)| {
            let mut w = webhook();
            f(&mut w);
            w.validate()
        };
        assert_eq!(t(|_| {}), Ok(()));
        assert_eq!(t(|w| w.url = "http://10.0.0.5:8080/hook".into()), Ok(()));
        // 오류 문구에 경로(비밀)가 없다
        let err = t(|w| w.url = "ftp://h/SECRETPATH".into()).unwrap_err();
        assert!(!err.to_string().contains("SECRETPATH"), "{err}");
        assert!(matches!(
            t(|w| w.url = "nonsense".into()),
            Err(TargetError::Url(_))
        ));
        assert!(matches!(
            t(|w| {
                w.headers.insert("Host".into(), "x".into());
            }),
            Err(TargetError::Header(_))
        ));
        assert!(matches!(
            t(|w| {
                w.headers.insert("Bad Name".into(), "x".into());
            }),
            Err(TargetError::Header(_))
        ));
        assert!(matches!(
            t(|w| {
                w.headers.insert("X-A".into(), "a\r\nb".into());
            }),
            Err(TargetError::Header(_))
        ));
        assert_eq!(
            t(|w| w.headers = (0..17).map(|i| (format!("X-{i}"), "v".into())).collect()),
            Err(TargetError::TooManyHeaders)
        );
        assert_eq!(
            t(|w| w.token = Some("t".into())),
            Err(TargetError::TokenUnused("slack"))
        );
        assert_eq!(
            t(|w| {
                w.format = WebhookFormat::Gotify;
                w.token = Some("t".into());
            }),
            Ok(())
        );
        assert_eq!(
            t(|w| {
                w.format = WebhookFormat::Ntfy;
                w.token = Some(String::new());
            }),
            Err(TargetError::Token)
        );
        assert_eq!(
            t(|w| w.ca_pem = Some("junk".into())),
            Err(TargetError::CaPem)
        );
    }

    #[test]
    fn records_round_trip() {
        let r = NotificationRecord {
            at: 1,
            node: "n1".into(),
            rule: None,
            subject: None,
            state: NotificationState::Test,
            target: "ops-mail".into(),
            ok: true,
            error: None,
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            json!({"at": 1, "node": "n1", "state": "test", "target": "ops-mail", "ok": true})
        );
        assert_eq!(serde_json::from_value::<NotificationRecord>(v).unwrap(), r);
    }
}
