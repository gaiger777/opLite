//! 백업 작업. 정한 시각(UTC)에 고른 VM들을 백업 풀에 백업하고, 보존 규칙([`prune`])에 맞지 않는 옛
//! 백업을 지운다. 실행은 리더의 스케줄러가 한다.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec, validate_name};
use crate::vm::Vm;

pub type BackupJob = Resource<BackupJobSpec, BackupJobStatus>;

const DAY: u64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct BackupJobSpec {
    /// 백업할 VM
    pub vms: VmSelector,
    /// 백업 풀 이름
    pub pool: String,
    pub schedule: JobSchedule,
    /// 보존 규칙. 모두 0이면 지우지 않는다.
    #[serde(default)]
    pub keep: Keep,
    /// 끄면 예약 실행을 하지 않는다.
    #[serde(default = "yes")]
    #[cfg_attr(feature = "openapi", schema(default = true))]
    pub enabled: bool,
    /// 만든 백업을 보낼 원격 클러스터의 백업 풀. 없으면 보내지 않는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false, default))]
    pub replicate: Option<ReplicateTarget>,
    /// 0보다 크면 증분으로 백업하되(할 수 있으면) 체인의 증분이 이만큼 쌓이면 다음은 전체로 한다.
    /// 0이면 늘 전체다(뺀다).
    #[serde(default, skip_serializing_if = "is_zero")]
    #[cfg_attr(feature = "openapi", schema(default))]
    pub full_every: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// 백업을 보낼 곳: 원격 `remote`의 백업 풀 `pool`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ReplicateTarget {
    /// 원격 이름
    pub remote: String,
    /// 원격의 백업 풀 이름
    pub pool: String,
}

fn yes() -> bool {
    true
}

impl ResourceSpec for BackupJobSpec {
    const KIND: Kind = Kind::BackupJob;
    type Status = BackupJobStatus;
}

impl BackupJobSpec {
    pub fn validate(&self) -> Result<(), BackupJobError> {
        match &self.vms {
            VmSelector::All => {}
            VmSelector::Names(names) if names.is_empty() => return Err(BackupJobError::NoNames),
            VmSelector::Labels(labels) if labels.is_empty() => {
                return Err(BackupJobError::NoLabels);
            }
            VmSelector::Names(_) | VmSelector::Labels(_) => {}
        }
        if parse_at(&self.schedule.at).is_none() {
            return Err(BackupJobError::At);
        }
        let mut days = BTreeSet::new();
        for day in &self.schedule.days {
            if !days.insert(day) {
                return Err(BackupJobError::DuplicateDay(day.as_str()));
            }
        }
        if let Some(target) = &self.replicate {
            if validate_name(&target.remote).is_err() {
                return Err(BackupJobError::ReplicateRemote(target.remote.clone()));
            }
            if validate_name(&target.pool).is_err() {
                return Err(BackupJobError::ReplicatePool(target.pool.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BackupJobError {
    #[error("vms.names must not be empty")]
    NoNames,
    #[error("vms.labels must not be empty")]
    NoLabels,
    #[error("schedule.at must be HH:MM between 00:00 and 23:59")]
    At,
    #[error("schedule.days lists {0} more than once")]
    DuplicateDay(&'static str),
    #[error("replicate.remote {0:?} is not a valid remote name")]
    ReplicateRemote(String),
    #[error("replicate.pool {0:?} is not a valid storage pool name")]
    ReplicatePool(String),
}

/// 백업할 VM 고르기. JSON은 `{"all": true}`, `{"names": [...]}`, `{"labels": {...}}` 중 하나다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "VmSelectorRepr", into = "VmSelectorRepr")]
pub enum VmSelector {
    /// 모든 VM
    All,
    /// 이 이름의 VM들
    Names(Vec<String>),
    /// 이 라벨을 모두 가진 VM들
    Labels(BTreeMap<String, String>),
}

impl VmSelector {
    pub fn matches(&self, vm: &Vm) -> bool {
        match self {
            VmSelector::All => true,
            VmSelector::Names(names) => names.contains(&vm.name),
            VmSelector::Labels(labels) => labels.iter().all(|(k, v)| vm.labels.get(k) == Some(v)),
        }
    }
}

/// [`VmSelector`]의 JSON 모양
#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct VmSelectorRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    all: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    labels: Option<BTreeMap<String, String>>,
}

impl TryFrom<VmSelectorRepr> for VmSelector {
    type Error = &'static str;

    fn try_from(r: VmSelectorRepr) -> Result<Self, Self::Error> {
        match (r.all, r.names, r.labels) {
            (Some(true), None, None) => Ok(VmSelector::All),
            (Some(false), None, None) => Err("vms.all must be true"),
            (None, Some(names), None) => Ok(VmSelector::Names(names)),
            (None, None, Some(labels)) => Ok(VmSelector::Labels(labels)),
            _ => Err("vms needs exactly one of all, names or labels"),
        }
    }
}

impl From<VmSelector> for VmSelectorRepr {
    fn from(s: VmSelector) -> Self {
        match s {
            VmSelector::All => VmSelectorRepr {
                all: Some(true),
                ..Default::default()
            },
            VmSelector::Names(names) => VmSelectorRepr {
                names: Some(names),
                ..Default::default()
            },
            VmSelector::Labels(labels) => VmSelectorRepr {
                labels: Some(labels),
                ..Default::default()
            },
        }
    }
}

// JSON 모양이 enum과 달라서 API 문서 스키마를 손으로 적는다: 세 객체 중 하나.
#[cfg(feature = "openapi")]
impl utoipa::PartialSchema for VmSelector {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::RefOr;
        use utoipa::openapi::schema::{ObjectBuilder, OneOfBuilder, Schema};
        let one = |field: &str, schema: RefOr<Schema>| {
            ObjectBuilder::new().property(field, schema).required(field)
        };
        OneOfBuilder::new()
            .item(one("all", bool::schema()))
            .item(one("names", Vec::<String>::schema()))
            .item(one("labels", BTreeMap::<String, String>::schema()))
            .into()
    }
}

#[cfg(feature = "openapi")]
impl utoipa::ToSchema for VmSelector {}

/// 실행 일정: `days`의 `at`(UTC). `days`가 비면 매일.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct JobSchedule {
    /// `HH:MM`(UTC)
    #[cfg_attr(feature = "openapi", schema(example = "03:00"))]
    pub at: String,
    #[serde(default)]
    pub days: Vec<Weekday>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl Weekday {
    pub fn as_str(self) -> &'static str {
        match self {
            Weekday::Mon => "mon",
            Weekday::Tue => "tue",
            Weekday::Wed => "wed",
            Weekday::Thu => "thu",
            Weekday::Fri => "fri",
            Weekday::Sat => "sat",
            Weekday::Sun => "sun",
        }
    }
}

/// `HH:MM`을 하루 안의 초로. 두 자리씩, 00:00~23:59만.
fn parse_at(at: &str) -> Option<u64> {
    let (h, m) = at.split_once(':')?;
    let two = |s: &str| {
        (s.len() == 2 && s.bytes().all(|c| c.is_ascii_digit()))
            .then(|| s.parse::<u64>().ok())
            .flatten()
    };
    let (h, m) = (two(h)?, two(m)?);
    (h < 24 && m < 60).then_some(h * 3600 + m * 60)
}

/// `after`보다 뒤의 첫 실행 시각(유닉스 초). 검증한 일정을 받는다.
pub fn next_run(schedule: &JobSchedule, after: u64) -> u64 {
    let at = parse_at(&schedule.at).unwrap_or(0);
    let mut day = after / DAY;
    loop {
        let t = day * DAY + at;
        // 1970-01-01은 목요일이다.
        let weekday = [
            Weekday::Thu,
            Weekday::Fri,
            Weekday::Sat,
            Weekday::Sun,
            Weekday::Mon,
            Weekday::Tue,
            Weekday::Wed,
        ][(day % 7) as usize];
        if t > after && (schedule.days.is_empty() || schedule.days.contains(&weekday)) {
            return t;
        }
        day += 1;
    }
}

/// 보존 규칙: 최신 `last`개, 그다음 날(UTC)·ISO 주·달마다 가장 새 것 하나씩 `daily`·`weekly`·`monthly`개.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default, deny_unknown_fields)]
pub struct Keep {
    pub last: u32,
    pub daily: u32,
    pub weekly: u32,
    pub monthly: u32,
}

/// 보존 규칙에 맞지 않아 지울 백업(입력 순서). `backups`는 (백업 id, 만든 시각)이고 id(v7)가 클수록
/// 새 것으로 본다. 규칙이 모두 0이면 지우지 않는다.
pub fn prune(backups: &[(Uuid, u64)], keep: &Keep) -> Vec<Uuid> {
    if *keep == Keep::default() {
        return Vec::new();
    }
    // 최신부터. 순서는 id(v7, 만든 순서)로 정하고 기간은 만든 시각으로 나눈다.
    let mut sorted: Vec<(Uuid, u64)> = backups.to_vec();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.0));
    let mut kept: BTreeSet<Uuid> = sorted
        .iter()
        .take(keep.last as usize)
        .map(|b| b.0)
        .collect();
    // 규칙마다 기간: 날(UTC)·ISO 주·달
    let rules: [(u32, PeriodOf); 3] = [
        (keep.daily, |d| (d.year(), d.ordinal().into())),
        (keep.weekly, |d| {
            let (year, week, _) = d.to_iso_week_date();
            (year, week.into())
        }),
        (keep.monthly, |d| (d.year(), u8::from(d.month()).into())),
    ];
    for (n, period) in rules {
        let mut seen = BTreeSet::new();
        let mut count = 0;
        for (id, t) in &sorted {
            if count >= n {
                break;
            }
            // 앞 규칙이 이미 남긴 백업도 그 기간은 쓴 것으로 친다.
            if seen.insert(period(date(*t))) && kept.insert(*id) {
                count += 1;
            }
        }
    }
    backups
        .iter()
        .map(|b| b.0)
        .filter(|id| !kept.contains(id))
        .collect()
}

/// 날짜가 든 보존 기간(연과 그 안의 날·주·달 번호)
type PeriodOf = fn(time::Date) -> (i32, u32);

/// 유닉스 초의 날짜(UTC)
fn date(t: u64) -> time::Date {
    time::OffsetDateTime::from_unix_timestamp(t as i64)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .date()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(default)]
pub struct BackupJobStatus {
    /// 다음 예약 실행 시각(유닉스 초)
    pub next_run: Option<u64>,
    /// 진행 중인 실행
    pub run: Option<JobRun>,
    /// 마지막으로 끝난 실행
    pub last_run: Option<JobRunResult>,
    /// 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM과 이유
    pub errors: Vec<JobError>,
}

/// 진행 중인 실행
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobRun {
    /// 시작한 시각(유닉스 초)
    pub started_at: u64,
    /// 아직 백업을 시작하지 않은 VM id
    #[serde(default)]
    pub queue: Vec<Uuid>,
    /// 이번 실행에서 만든 백업 id
    #[serde(default)]
    pub backups: Vec<Uuid>,
}

/// 끝난 실행의 결과
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobRunResult {
    pub started_at: u64,
    pub finished_at: u64,
    /// 성공한 백업 수
    pub ok: u32,
    /// 실패한 백업과 백업하지 못한 VM 수
    pub failed: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct JobError {
    /// VM 이름
    pub vm: String,
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(v: serde_json::Value) -> Result<BackupJobSpec, serde_json::Error> {
        serde_json::from_value(v)
    }

    fn base() -> serde_json::Value {
        json!({
            "vms": { "all": true },
            "pool": "backups",
            "schedule": { "at": "03:00" }
        })
    }

    /// 2026-11-02(월) 00:00 UTC
    const MON: u64 = 1_793_577_600;

    fn at(day: u64, h: u64, m: u64) -> u64 {
        MON + day * DAY + h * 3600 + m * 60
    }

    fn schedule(at: &str, days: &[Weekday]) -> JobSchedule {
        JobSchedule {
            at: at.into(),
            days: days.to_vec(),
        }
    }

    #[test]
    fn spec_defaults_and_round_trips() {
        let s = spec(base()).unwrap();
        assert_eq!(s.vms, VmSelector::All);
        assert_eq!(s.keep, Keep::default());
        assert!(s.enabled);
        assert!(s.schedule.days.is_empty());
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json,
            json!({
                "vms": { "all": true },
                "pool": "backups",
                "schedule": { "at": "03:00", "days": [] },
                "keep": { "last": 0, "daily": 0, "weekly": 0, "monthly": 0 },
                "enabled": true
            })
        );
        assert_eq!(spec(json).unwrap(), s);

        let full = spec(json!({
            "vms": { "labels": { "tier": "prod" } },
            "pool": "backups",
            "schedule": { "at": "23:59", "days": ["mon", "sun"] },
            "keep": { "last": 3, "monthly": 6 },
            "enabled": false
        }))
        .unwrap();
        assert_eq!(full.schedule.days, vec![Weekday::Mon, Weekday::Sun]);
        assert_eq!(
            full.keep,
            Keep {
                last: 3,
                daily: 0,
                weekly: 0,
                monthly: 6
            }
        );
        assert!(!full.enabled);
        assert_eq!(full.validate(), Ok(()));
    }

    #[test]
    fn replicate_round_trips_and_is_left_out_when_off() {
        // 없으면 빼서 옛 노드가 읽는 모양 그대로다.
        let s = spec(base()).unwrap();
        assert_eq!(s.replicate, None);
        assert!(serde_json::to_value(&s).unwrap().get("replicate").is_none());

        let mut with = base();
        with["replicate"] = json!({ "remote": "dr", "pool": "backups" });
        let s = spec(with).unwrap();
        assert_eq!(
            s.replicate,
            Some(ReplicateTarget {
                remote: "dr".into(),
                pool: "backups".into()
            })
        );
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["replicate"],
            json!({ "remote": "dr", "pool": "backups" })
        );
        assert_eq!(spec(json).unwrap(), s);

        for bad in [
            json!({ "remote": "dr" }),
            json!({ "remote": "dr", "pool": "backups", "node": "a" }),
        ] {
            let mut j = base();
            j["replicate"] = bad.clone();
            assert!(spec(j).is_err(), "{bad}");
        }
    }

    #[test]
    fn full_every_round_trips_and_is_left_out_when_zero() {
        // 0이면 빼서 옛 노드가 읽는 모양 그대로다.
        let s = spec(base()).unwrap();
        assert_eq!(s.full_every, 0);
        assert!(
            serde_json::to_value(&s)
                .unwrap()
                .get("full_every")
                .is_none()
        );

        let mut with = base();
        with["full_every"] = json!(6);
        let s = spec(with).unwrap();
        assert_eq!(s.full_every, 6);
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["full_every"], 6);
        assert_eq!(spec(json).unwrap(), s);
    }

    #[test]
    fn validates_replicate_names() {
        let with = |remote: &str, pool: &str| {
            let mut s = spec(base()).unwrap();
            s.replicate = Some(ReplicateTarget {
                remote: remote.into(),
                pool: pool.into(),
            });
            s.validate()
        };
        assert_eq!(with("dr-1", "backups"), Ok(()));
        assert_eq!(
            with("", "backups"),
            Err(BackupJobError::ReplicateRemote(String::new()))
        );
        assert_eq!(
            with("DR", "backups"),
            Err(BackupJobError::ReplicateRemote("DR".into()))
        );
        assert_eq!(
            with("dr", "../x"),
            Err(BackupJobError::ReplicatePool("../x".into()))
        );
        assert_eq!(
            BackupJobError::ReplicatePool("a/b".into()).to_string(),
            "replicate.pool \"a/b\" is not a valid storage pool name"
        );
    }

    #[test]
    fn selector_is_exactly_one_of_three_shapes() {
        let names = VmSelector::Names(vec!["web".into(), "db".into()]);
        let labels = VmSelector::Labels([("tier".to_string(), "prod".to_string())].into());
        for (sel, j) in [
            (VmSelector::All, json!({ "all": true })),
            (names, json!({ "names": ["web", "db"] })),
            (labels, json!({ "labels": { "tier": "prod" } })),
        ] {
            assert_eq!(serde_json::to_value(&sel).unwrap(), j);
            assert_eq!(serde_json::from_value::<VmSelector>(j).unwrap(), sel);
        }
        for bad in [
            json!({}),
            json!({ "all": false }),
            json!({ "all": true, "names": ["web"] }),
            json!({ "names": ["web"], "labels": { "a": "b" } }),
            json!({ "name": ["web"] }),
            json!("all"),
        ] {
            assert!(
                serde_json::from_value::<VmSelector>(bad.clone()).is_err(),
                "{bad}"
            );
        }
        let err = serde_json::from_value::<VmSelector>(json!({})).unwrap_err();
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn spec_rejects_unknown_fields() {
        for bad in [
            json!({ "vms": { "all": true }, "pool": "b", "schedule": { "at": "03:00" }, "poll": 1 }),
            json!({ "vms": { "all": true }, "pool": "b", "schedule": { "at": "03:00", "day": [] } }),
            json!({ "vms": { "all": true }, "pool": "b", "schedule": { "at": "03:00" }, "keep": { "hourly": 1 } }),
            json!({ "vms": { "all": true }, "pool": "b", "schedule": { "at": "03:00", "days": ["monday"] } }),
        ] {
            assert!(spec(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn validates_selector_time_and_days() {
        let with = |f: &dyn Fn(&mut BackupJobSpec)| {
            let mut s = spec(base()).unwrap();
            f(&mut s);
            s.validate()
        };
        for ok in ["00:00", "23:59", "09:05"] {
            assert_eq!(with(&|s| s.schedule.at = ok.into()), Ok(()), "{ok}");
        }
        for bad in [
            "24:00", "23:60", "3:00", "03:0", "0300", "03:00:00", "", "ab:cd", "+3:00", "03:-1",
        ] {
            assert_eq!(
                with(&|s| s.schedule.at = bad.into()),
                Err(BackupJobError::At),
                "{bad}"
            );
        }
        assert_eq!(
            with(&|s| s.schedule.days = vec![Weekday::Mon, Weekday::Wed, Weekday::Mon]),
            Err(BackupJobError::DuplicateDay("mon"))
        );
        assert_eq!(
            with(&|s| s.vms = VmSelector::Names(vec![])),
            Err(BackupJobError::NoNames)
        );
        assert_eq!(
            with(&|s| s.vms = VmSelector::Labels(BTreeMap::new())),
            Err(BackupJobError::NoLabels)
        );
        assert_eq!(
            BackupJobError::At.to_string(),
            "schedule.at must be HH:MM between 00:00 and 23:59"
        );
    }

    #[test]
    fn selector_matches_vms() {
        let mut vm: Vm = serde_json::from_value(json!({
            "id": Uuid::now_v7(),
            "name": "web",
            "revision": 1,
            "labels": { "tier": "prod", "team": "a" },
            "spec": { "cpu": { "cores": 1 }, "memory_mib": 512, "machine": "virt" },
            "status": {}
        }))
        .unwrap();
        let labels = |pairs: &[(&str, &str)]| {
            VmSelector::Labels(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            )
        };
        assert!(VmSelector::All.matches(&vm));
        assert!(VmSelector::Names(vec!["db".into(), "web".into()]).matches(&vm));
        assert!(!VmSelector::Names(vec!["db".into()]).matches(&vm));
        assert!(labels(&[("tier", "prod")]).matches(&vm));
        assert!(labels(&[("tier", "prod"), ("team", "a")]).matches(&vm));
        assert!(!labels(&[("tier", "prod"), ("team", "b")]).matches(&vm));
        assert!(!labels(&[("tier", "dev")]).matches(&vm));
        vm.labels.clear();
        assert!(!labels(&[("tier", "prod")]).matches(&vm));
    }

    #[test]
    fn next_run_table() {
        use Weekday::*;
        let every = schedule("03:00", &[]);
        assert_eq!(every.days, vec![]);
        for (sched, after, want) in [
            // 같은 날 뒤
            (&every, at(0, 1, 0), at(0, 3, 0)),
            // 이미 지났으면 다음 날
            (&every, at(0, 4, 0), at(1, 3, 0)),
            // 정각이면 다음 날(after보다 뒤)
            (&every, at(0, 3, 0), at(1, 3, 0)),
            (&every, at(0, 3, 0) - 1, at(0, 3, 0)),
            // 요일 건너뛰기: 월요일 뒤 → 수요일
            (&schedule("03:00", &[Mon, Wed]), at(0, 4, 0), at(2, 3, 0)),
            // 일요일 뒤 → 다음 주 월요일
            (&schedule("03:00", &[Mon]), at(0, 3, 0), at(7, 3, 0)),
            (&schedule("03:00", &[Sun]), at(0, 0, 0), at(6, 3, 0)),
            // 자정
            (&schedule("00:00", &[]), at(0, 0, 0), at(1, 0, 0)),
            (&schedule("00:00", &[]), at(0, 23, 59), at(1, 0, 0)),
            (&schedule("00:00", &[Tue]), at(0, 0, 0), at(1, 0, 0)),
            // 하루의 끝
            (&schedule("23:59", &[]), at(0, 23, 58) + 59, at(0, 23, 59)),
            (&schedule("23:59", &[Sat]), at(5, 23, 59), at(12, 23, 59)),
        ] {
            assert_eq!(next_run(sched, after), want, "{sched:?} after {after}");
        }
        // 1970-01-01(목) 기준이 맞는지: 2026-11-02는 월요일
        assert_eq!(next_run(&schedule("00:00", &[Mon]), MON - 1), MON);
    }

    /// 만든 시각 순서인 id(같은 시각이면 입력 순서). 백업 id(v7)처럼 만든 순서를 따른다.
    fn ids(times: &[u64]) -> Vec<Uuid> {
        times
            .iter()
            .enumerate()
            .map(|(i, t)| Uuid::from_u128(((*t as u128) << 32) | i as u128))
            .collect()
    }

    /// 지울 것의 위치(입력 순서)
    fn pruned(times: &[u64], keep: Keep) -> Vec<usize> {
        let ids = ids(times);
        let backups: Vec<(Uuid, u64)> = ids.iter().copied().zip(times.iter().copied()).collect();
        prune(&backups, &keep)
            .iter()
            .map(|id| ids.iter().position(|x| x == id).unwrap())
            .collect()
    }

    fn keep(last: u32, daily: u32, weekly: u32, monthly: u32) -> Keep {
        Keep {
            last,
            daily,
            weekly,
            monthly,
        }
    }

    #[test]
    fn prune_keeps_last_n() {
        // 순서와 상관없이 가장 새 것을 남긴다.
        let times = [at(0, 3, 0), at(2, 3, 0), at(1, 3, 0), at(3, 3, 0)];
        assert_eq!(pruned(&times, keep(2, 0, 0, 0)), vec![0, 2]);
        assert_eq!(pruned(&times, keep(4, 0, 0, 0)), Vec::<usize>::new());
        assert_eq!(pruned(&times, keep(9, 0, 0, 0)), Vec::<usize>::new());
        assert_eq!(pruned(&[], keep(1, 0, 0, 0)), Vec::<usize>::new());
    }

    #[test]
    fn prune_orders_by_id_and_classifies_by_created_at() {
        // 시계가 거꾸로 가서 나중에 만든 백업(id가 큰 것)의 만든 시각이 더 이르다: id가 새 것이 남는다.
        let (old, new) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let backups = [(old, at(1, 3, 0)), (new, at(0, 3, 0))];
        assert_eq!(prune(&backups, &keep(1, 0, 0, 0)), vec![old]);
        // 기간은 만든 시각으로 나눈다: 둘은 다른 날이므로 daily 2는 둘 다 남긴다.
        assert_eq!(prune(&backups, &keep(0, 2, 0, 0)), Vec::<Uuid>::new());
        // 같은 날이면 id가 큰 것이 그날을 쓴다.
        let same_day = [(old, at(0, 9, 0)), (new, at(0, 1, 0))];
        assert_eq!(prune(&same_day, &keep(0, 1, 0, 0)), vec![old]);
    }

    #[test]
    fn prune_keeps_nothing_away_when_all_zero() {
        let times = [at(0, 3, 0), at(1, 3, 0), at(2, 3, 0)];
        assert_eq!(pruned(&times, Keep::default()), Vec::<usize>::new());
    }

    #[test]
    fn prune_daily_keeps_newest_of_each_day() {
        // 같은 날 여러 개: 그날 가장 새 것만
        let times = [
            at(0, 1, 0),
            at(0, 9, 0),
            at(1, 1, 0),
            at(1, 9, 0),
            at(2, 1, 0),
            at(2, 9, 0),
        ];
        assert_eq!(pruned(&times, keep(0, 2, 0, 0)), vec![0, 1, 2, 4]);
        assert_eq!(pruned(&times, keep(0, 9, 0, 0)), vec![0, 2, 4]);
        // last가 남긴 것도 그날을 쓴 것으로 친다: 2일 9시를 last가 남기면 daily 2개는 1일·0일.
        assert_eq!(pruned(&times, keep(1, 2, 0, 0)), vec![0, 2, 4]);
        // last가 2일 두 개를 남기면 daily는 1일과 0일
        assert_eq!(pruned(&times, keep(2, 2, 0, 0)), vec![0, 2]);
    }

    #[test]
    fn prune_combines_daily_weekly_monthly() {
        // 2026-11-02(월)부터 매일 하나씩 40일: 11-02 ~ 12-11
        let times: Vec<u64> = (0..40).map(|d| at(d, 3, 0)).collect();
        let kept = |k: Keep| {
            let gone = pruned(&times, k);
            (0..40)
                .filter(|i| !gone.contains(i))
                .collect::<Vec<usize>>()
        };
        // daily 3: 12-11·10·09
        assert_eq!(kept(keep(0, 3, 0, 0)), vec![37, 38, 39]);
        // weekly 3: 각 ISO 주의 가장 새 것(일요일), 이번 주는 12-11(금)
        // 12-11(39, 주 50), 12-06(34, 주 49 일), 11-29(27, 주 48 일)
        assert_eq!(kept(keep(0, 0, 3, 0)), vec![27, 34, 39]);
        // monthly 2: 12-11, 11-30(28)
        assert_eq!(kept(keep(0, 0, 0, 2)), vec![28, 39]);
        // daily 3이 이번 주를 쓰므로 weekly 2는 주 49·48의 일요일
        assert_eq!(kept(keep(0, 3, 2, 0)), vec![27, 34, 37, 38, 39]);
        // 모두: last 1(39), daily 2(38·37), weekly 2(34·27), monthly 2: 12월은 이미 썼고 11월 30일(28)·10월 없음
        assert_eq!(kept(keep(1, 2, 2, 2)), vec![27, 28, 34, 37, 38, 39]);
    }

    #[test]
    fn prune_weekly_uses_iso_weeks_across_years() {
        // 2026-12-31(목)과 2027-01-03(일)은 ISO 2026-W53, 2027-01-04(월)는 2027-W01
        let dec31 = 1_798_675_200; // 2026-12-31 00:00 UTC
        let times = [dec31, dec31 + 3 * DAY, dec31 + 4 * DAY];
        assert_eq!(pruned(&times, keep(0, 0, 2, 0)), vec![0]);
        // 달로는 2026-12와 2027-01
        assert_eq!(pruned(&times, keep(0, 0, 0, 2)), vec![1]);
    }

    #[test]
    fn status_defaults_and_serializes() {
        let st: BackupJobStatus = serde_json::from_value(json!({})).unwrap();
        assert_eq!(st, BackupJobStatus::default());
        let vm = Uuid::now_v7();
        let full = BackupJobStatus {
            next_run: Some(MON),
            run: Some(JobRun {
                started_at: MON,
                queue: vec![vm],
                backups: vec![],
            }),
            last_run: Some(JobRunResult {
                started_at: 1,
                finished_at: 2,
                ok: 3,
                failed: 1,
            }),
            errors: vec![JobError {
                vm: "web".into(),
                error: "a backup of this VM is already running".into(),
            }],
        };
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(
            json,
            json!({
                "next_run": MON,
                "run": { "started_at": MON, "queue": [vm], "backups": [] },
                "last_run": { "started_at": 1, "finished_at": 2, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "web", "error": "a backup of this VM is already running" }]
            })
        );
        assert_eq!(
            serde_json::from_value::<BackupJobStatus>(json).unwrap(),
            full
        );
    }

    #[test]
    fn kind_is_backup_job() {
        assert_eq!(serde_json::to_value(Kind::BackupJob).unwrap(), "backup-job");
        assert_eq!(Kind::BackupJob.as_str(), "backup-job");
    }

    #[cfg(feature = "openapi")]
    #[test]
    fn openapi_shows_selector_as_one_of_three_objects() {
        let schema = serde_json::to_value(<VmSelector as utoipa::PartialSchema>::schema()).unwrap();
        let items = schema["oneOf"].as_array().expect("oneOf");
        let required: Vec<_> = items.iter().map(|i| i["required"].clone()).collect();
        assert_eq!(
            required,
            vec![json!(["all"]), json!(["names"]), json!(["labels"])],
            "{schema}"
        );
    }
}
