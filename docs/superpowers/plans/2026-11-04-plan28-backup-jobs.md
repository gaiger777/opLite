# opLite 계획 28: 백업 작업과 보존 구현 계획

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 대상 VM·시각·보존 규칙을 정한 백업 작업을 리더가 정해진 시각에 돌리고, 규칙에서 벗어난 오래된 백업을 지운다. 하위 프로젝트 7(백업)을 마친다.

**Architecture:** `BackupJob` 리소스(대상 VM 고르기, 백업 풀, UTC 시각·요일, Proxmox식 보존 규칙)를 둔다. 리더의 `BackupJobScheduler`가 tick마다 때가 된 작업의 실행을 시작해 상태(`run.queue`·`run.backups`)에 적고, 노드당 동시 실행 제한 안에서 계획 27의 백업 만들기로 VM마다 `job`이 붙은 백업을 요청한다. 이번 실행의 백업이 모두 끝나면 결과를 적고 VM마다 보존 규칙으로 지울 백업을 계획 27의 지우기로 지운다. 상태가 Raft에 있으므로 리더가 바뀌어도 새 리더가 이어 간다.

**Tech Stack:** Rust 2024(MSRV 1.88), openraft 0.9, axum 0.8, utoipa, clap; 웹은 계획 8~27과 같다. 새 외부 의존성은 없다.

**설계 문서:** `docs/superpowers/specs/2026-11-04-backup-design.md` (§3과 §4·§5의 계획 28 부분, "구현(계획 28)" 메모 포함)
**선행 계획:** 계획 1~27 (완료)

---

## 범위

| 포함 | 제외 (담당) |
|---|---|
| `BackupJob` 리소스·API(만들기·바꾸기·지우기·지금 실행) | 증분·중복 제거·암호화·원격 저장소 |
| 다음 실행 시각(UTC), VM 고르기(전부·이름·라벨), Proxmox식 보존 규칙(last·daily·weekly·monthly) | 시간대·일광 절약 시간, 분 단위보다 촘촘한 일정 |
| 리더 스케줄러(놓친 실행 한 번, 노드당 동시 제한, 이번 실행 백업 이어받기, 보존 정리) | 멈춘 백업 Task의 시간 제한 |
| oplctl·웹, 실제 시험, README, CI storage-it 시험 | |

## 이전 계획에서 이어받는 사항

- 계획 27: `Backup` 리소스(`spec.job`은 이 계획에서 채운다), 백업 만들기 검사·노드 고르기·Task 만들기, 지우기(파일이 있는 노드로), 백업이 진행 중이면 VM을 붙잡는 규칙.
- 리더 루프(`SnapshotScheduler`, `HaManager`, `ClusterJanitor`): 리더만, tick, opld가 띄우고 종료.

## 실행 환경

계획 27과 같다. 스케줄러 시험(`opl-api` `storage-it` `tests/backup_job_runs.rs`)과 실제 시험(`opld` `qemu-it` `tests/backup.rs`)은 이 Mac(qemu-img·QEMU)에서 돈다. 이 계획이 CI `check` 작업의 storage-it 단계에 백업 시험들을 더한다.

생성 파일은 이 문서에 싣지 않고 명령으로 만든다: `web/src/api/schema.d.ts`(`pnpm -C web gen:api`).

## 설계 결정

- **상태는 Raft에:** 실행의 큐와 만든 백업 목록을 작업 상태에 적어 리더가 바뀌어도 이어 간다. 상태 쓰기가 실패해도 다음 tick에 이번 실행의 백업을 이어받아 중복을 만들지 않는다.
- **보존은 안전하게:** 이 작업이 만든 ready 백업만, VM마다 따로 본다. 손으로 만든 백업·다른 작업의 백업·만드는 중인 백업은 건드리지 않는다. 0이 아닌 규칙은 늘 최신 백업을 남긴다. "최신"은 백업 id(UUIDv7) 순이라 노드 시계가 어긋나도 흔들리지 않는다.
- **실행은 끝난다:** Task가 끝났는데 pending인 백업(죽은 노드의 Task를 janitor가 실패 처리한 경우)은 실패로 세서 실행이 영영 열려 있지 않게 한다.
- **UTC:** 일정과 표시는 모두 UTC다. 놓친 실행은 한 번만 돈다.

## 파일 구조

```
crates/opl-types/src/{backup_job.rs, resource.rs, lib.rs}               # BackupJob, next_run, prune
crates/opl-api/src/{backup_jobs.rs, backups.rs, resources.rs, auth.rs, watch.rs, lib.rs}, tests/{backup_jobs.rs, backup_job_runs.rs}
crates/opld/src/{config.rs, member.rs}, tests/backup.rs                 # [backup] 설정, 실제 시험
crates/oplctl/src/cli.rs, web/src/{backups, api, auth, layout, i18n}/...
.github/workflows/ci.yml, README.md, docs/superpowers/specs/{2026-11-04-backup-design.md, 2026-10-20-opl-node-core-design.md}
```

## 편집 블록 읽는 법

코드는 Task마다 "`파일`을 만든다"(전체 내용), "`파일` 전체"(바꾼 뒤의 전체 내용), "`파일`에서 … 을 다음으로 바꾼다"(앞 블록을 찾아 뒤 블록으로 바꾼다)로 싣는다. 바꾸는 블록은 위에서부터 차례로 적용하며, 찾을 코드는 그때의 파일에서 줄 단위로 한 번만 나온다. 번역 파일(`ko.json`, `en.json`)은 두 언어의 키가 같아야 한다(테스트가 본다).

---

### Task 1: 백업 작업 타입과 순수 함수

opl-types `backup_job.rs`: `BackupJobSpec { vms, pool, schedule, keep, enabled }` — `vms`는 JSON `{"all": true}`·`{"names": [...]}`·`{"labels": {...}}` 중 하나(`VmSelector`, OpenAPI는 oneOf), `schedule { at: "HH:MM"(UTC), days }`, `keep { last, daily, weekly, monthly }`. 상태 `{ next_run, run { started_at, queue, backups }, last_run { started_at, finished_at, ok, failed }, errors }`. `Kind::BackupJob`(`backup-job`). 순수 함수: `next_run(schedule, after)`(after보다 뒤의 첫 시각), `VmSelector::matches`, `prune(백업들, keep)`(Proxmox 규칙 — 최신은 백업 id 순, 기간은 `created_at`의 UTC 날짜·ISO 주·연월; 앞 규칙이 남긴 백업의 기간도 쓴 것으로 셈; 모두 0이면 지우지 않음).

**Files:**
- Create: `crates/opl-types/src/backup_job.rs`
- Modify: `crates/opl-types/src/lib.rs`
- Modify: `crates/opl-types/src/resource.rs`

- [ ] **Step 1: 코드**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-types/src/backup_job.rs`를 만든다:

```rust
//! 백업 작업. 정한 시각(UTC)에 고른 VM들을 백업 풀에 백업하고, 보존 규칙([`prune`])에 맞지 않는 옛
//! 백업을 지운다. 실행은 리더의 스케줄러가 한다.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::resource::{Kind, Resource, ResourceSpec};
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
```

`crates/opl-types/src/lib.rs`에서

```rust
pub mod backup;
pub mod cluster;
```

을 다음으로 바꾼다:

```rust
pub mod backup;
pub mod backup_job;
pub mod cluster;
```

`crates/opl-types/src/resource.rs`에서

```rust
    Backup,
}
```

을 다음으로 바꾼다:

```rust
    Backup,
    BackupJob,
}
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Backup => "backup",
        }
```

을 다음으로 바꾼다:

```rust
            Kind::Backup => "backup",
            Kind::BackupJob => "backup-job",
        }
```

`crates/opl-types/src/resource.rs`에서

```rust
            Kind::Backup,
        ] {
```

을 다음으로 바꾼다:

```rust
            Kind::Backup,
            Kind::BackupJob,
        ] {
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 991개, 실패 0

Run: `cargo test -p opl-types backup_job`
Expected: PASS

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opl-types): 백업 작업 타입과 순수 함수 — BackupJob(VmSelector·JobSchedule·Keep·상태), Kind::BackupJob, next_run, VmSelector::matches, prune(보존 규칙)"
```

---

### Task 2: 백업 작업 API

`/backup-jobs` 목록·만들기(201, 풀이 백업 풀이어야, 요일은 주 순서로 정렬해 저장, `next_run` 계산)·읽기·`PATCH`(보낸 것만, `keep`은 통째, 일정이나 켜기가 바뀌면 `next_run`을 다시 계산하되 켜진 작업의 대기 중 "지금 실행"은 남김 — `next_run` 상태를 spec보다 먼저 씀)·지우기(백업은 남음)·`POST /backup-jobs/{key}/run`(409 `job_running`·`job_disabled`). operator가 쓸 수 있다. watch 종류 `backup-job`. 계획 27의 백업 만들기·지우기 Task 만들기를 `check_backup`·`start_backup`(이름 검사 포함)·`start_backup_delete`로 빼서 API와 스케줄러가 함께 쓴다.

**Files:**
- Create: `crates/opl-api/src/backup_jobs.rs`
- Create: `crates/opl-api/tests/backup_jobs.rs`
- Modify: `crates/opl-api/src/auth.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opl-api/src/resources.rs`
- Modify: `crates/opl-api/src/watch.rs`
- Modify: `crates/opl-api/tests/openapi.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/auth.rs`에서

```rust
                Action::Write | Action::Operate => {
                    matches!(kind, Kind::Vm | Kind::Volume | Kind::Backup)
                }
```

을 다음으로 바꾼다:

```rust
                Action::Write | Action::Operate => {
                    matches!(
                        kind,
                        Kind::Vm | Kind::Volume | Kind::Backup | Kind::BackupJob
                    )
                }
```

`crates/opl-api/src/auth.rs`에서

```rust
        assert!(a.check(&op, Action::Write, Kind::Backup));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

을 다음으로 바꾼다:

```rust
        assert!(a.check(&op, Action::Write, Kind::Backup));
        assert!(a.check(&op, Action::Write, Kind::BackupJob));
        assert!(!a.check(&op, Action::Write, Kind::StoragePool));
```

`crates/opl-api/src/backup_jobs.rs`를 만든다:

```rust
//! 백업 작업 만들기·바꾸기·지우기·지금 실행(설계 §3). 목록과 조회는 `resources`의 공통 핸들러다.
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobSchedule, Keep, VmSelector, next_run,
};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, check_name, if_match, lookup};
use crate::state::AppState;

/// 스펙 검증과 풀 검사: 풀이 있고 백업 풀이어야 한다(400). 요일은 주의 순서로 정리한다.
fn check_spec(state: &AppState, spec: &mut BackupJobSpec) -> Result<(), ApiError> {
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    spec.schedule.days.sort();
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&spec.pool)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {:?} not found", spec.pool)))?;
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    Ok(())
}

/// 작업을 만들고 다음 실행 시각(`status.next_run`)을 적는다. 이름이 겹치면 409 `name_taken`.
#[utoipa::path(post, path = "/backup-jobs", tag = "backups",
    request_body = CreateRequest<BackupJobSpec>,
    responses((status = 201, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut req): Json<CreateRequest<BackupJobSpec>>,
) -> Result<(StatusCode, Json<BackupJob>), ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    check_name(&req.name)?;
    check_spec(&state, &mut req.spec)?;
    let mut job: BackupJob = Resource::new(req.name, req.spec);
    job.labels = req.labels;
    job.status.next_run = Some(next_run(&job.spec.schedule, now()));
    job.revision = state.cluster.create(&job).await?;
    Ok((StatusCode::CREATED, Json(job)))
}

/// 백업 작업에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupJobPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub vms: Option<VmSelector>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub pool: Option<String>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub schedule: Option<JobSchedule>,
    /// 보존 규칙 전체를 이것으로 바꾼다(적지 않은 단위는 0).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub keep: Option<Keep>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
}

/// 스펙의 주어진 값을 바꾼다. 아무것도 없으면 400. 일정을 바꾸거나 작업을 다시 켜면 다음 실행 시각을
/// 지금부터 다시 계산한다(꺼 둔 사이 지난 실행은 하지 않는다). 다만 켜 둔 작업의 지금 실행 요청이
/// 기다리고 있으면(다음 실행이 지났으면) 그대로 둔다. 진행 중인 실행은 그대로 끝까지 간다.
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(patch, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = BackupJobPatch,
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<BackupJobPatch>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    if req.vms.is_none()
        && req.pool.is_none()
        && req.schedule.is_none()
        && req.keep.is_none()
        && req.enabled.is_none()
    {
        return Err(ApiError::bad_request(
            "give vms, pool, schedule, keep or enabled",
        ));
    }
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != job.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: job.revision,
            })
            .into());
        }
        let mut spec = job.spec.clone();
        if let Some(vms) = &req.vms {
            spec.vms = vms.clone();
        }
        if let Some(pool) = &req.pool {
            spec.pool = pool.clone();
        }
        if let Some(schedule) = &req.schedule {
            spec.schedule = schedule.clone();
        }
        if let Some(keep) = req.keep {
            spec.keep = keep;
        }
        if let Some(enabled) = req.enabled {
            spec.enabled = enabled;
        }
        check_spec(&state, &mut spec)?;
        if spec == job.spec {
            return Ok(Json(job));
        }
        let now = now();
        let requested = job.spec.enabled && job.status.next_run.is_some_and(|t| t <= now);
        let reschedule = (spec.schedule != job.spec.schedule
            || (spec.enabled && !job.spec.enabled))
            && !requested;
        // 다음 실행을 스펙보다 먼저 쓴다. 스펙 쓰기가 실패해도 스케줄러는 새 다음 실행(앞날)만 보므로
        // 옛 일정이나 꺼 둔 사이 지난 실행을 바로 돌리지 않는다.
        let mut revision = job.revision;
        if reschedule {
            let status = BackupJobStatus {
                next_run: Some(next_run(&spec.schedule, now)),
                ..job.status.clone()
            };
            match state
                .cluster
                .update_status_checked::<BackupJobSpec>(job.id, revision, &status)
                .await
            {
                Ok(r) => revision = r,
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                    if expected.is_none() =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
        }
        match state
            .cluster
            .update_spec(job.id, Some(revision), job.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(lookup::<BackupJobSpec>(
            &state.cluster,
            &job.id.to_string(),
        )?));
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업의 상태를 `change`가 돌려준 값으로 쓰고 쓴 작업을 돌려준다. 그 사이 스케줄러가 상태를 썼으면
/// 다시 읽어 바꾼다.
async fn update_status(
    state: &AppState,
    id: Uuid,
    change: impl Fn(&BackupJob) -> Result<BackupJobStatus, ApiError>,
) -> Result<BackupJob, ApiError> {
    for _ in 0..10 {
        let job = lookup::<BackupJobSpec>(&state.cluster, &id.to_string())?;
        let status = change(&job)?;
        match state
            .cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await
        {
            Ok(_) => return lookup::<BackupJobSpec>(&state.cluster, &id.to_string()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the backup job kept changing while being updated; try again",
    ))
}

/// 작업만 지운다. 작업이 만든 백업은 남는다(백업의 `job`도 그대로).
#[utoipa::path(delete, path = "/backup-jobs/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let job = lookup::<BackupJobSpec>(&state.cluster, &key)?;
    state
        .cluster
        .delete::<BackupJobSpec>(job.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 다음 실행 시각을 지금으로 당긴다. 리더의 스케줄러가 다음 차례에 실행을 시작한다. 실행이 진행
/// 중이면 409 `job_running`, 꺼 둔 작업이면 409 `job_disabled`.
#[utoipa::path(post, path = "/backup-jobs/{key}/run", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<BackupJobSpec, BackupJobStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn run_backup_job(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<BackupJob>, ApiError> {
    state.authorize(&principal, Action::Write, BackupJobSpec::KIND)?;
    let id = lookup::<BackupJobSpec>(&state.cluster, &key)?.id;
    let job = update_status(&state, id, |job| {
        if job.status.run.is_some() {
            return Err(ApiError::conflict(
                "job_running",
                format!("backup job {:?} is already running", job.name),
            ));
        }
        if !job.spec.enabled {
            return Err(ApiError::conflict(
                "job_disabled",
                format!("backup job {:?} is disabled; enable it first", job.name),
            ));
        }
        Ok(BackupJobStatus {
            next_run: Some(now()),
            ..job.status.clone()
        })
    })
    .await?;
    Ok(Json(job))
}
```

`crates/opl-api/src/backups.rs` 전체:

```rust
//! VM 백업 만들기·목록·읽기·지우기·복원(설계 §2). 만들기는 VM 쓰기, 지우기·복원은 백업 쓰기 권한이다.
//! 실제 작업은 Task 실행기가 한다: 백업은 VM의 노드, 지우기·복원은 백업 풀을 쓸 수 있는 노드(공유가
//! 아닌 풀의 백업은 파일을 쓴 노드).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus, default_backup_name};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::{Node, NodeSpec};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{
    StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec, is_shared,
    pool_kind_supports_snapshots, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Vm, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name, lookup};
use crate::snapshots::{check_disks, check_not_busy};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::{check_security_groups, is_stopped};
use crate::volumes::{check_pool_on, pool_node};

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBackupRequest {
    /// 백업 풀(`backups`를 켠 풀)
    pub pool: String,
    /// 백업 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreRequest {
    /// 새 VM의 이름
    pub name: String,
    /// 새 디스크를 둘 풀
    pub pool: String,
    /// 복원을 실행할 노드(이름이나 id). 없으면 백업 풀과 대상 풀을 함께 쓸 수 있는 노드다.
    pub node: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct BackupQuery {
    /// 이 VM(id나 이름)의 백업만. 이름이면 지금 그 이름의 VM 백업과 그 이름으로 남은(지운 VM의) 백업이다.
    pub vm: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DeleteBackupQuery {
    /// 파일을 지울 노드가 없어도 리소스만 지운다. 파일은 백업 풀에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 리소스만 지운 백업 지우기(`force`)의 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct BackupForgotten {
    pub message: String,
}

/// 지금 시각(유닉스 초)
pub(crate) fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// `fits`를 만족하는 온라인 노드 가운데 이름이 가장 앞선 노드
fn first_node(state: &AppState, fits: impl Fn(&Node) -> bool) -> Result<Option<Uuid>, ApiError> {
    let (_, mut nodes) = state.cluster.list::<NodeSpec>()?;
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(nodes
        .iter()
        .find(|n| n.status.online && fits(n))
        .map(|n| n.id))
}

/// 백업 `id`를 다루는 끝나지 않은 Task가 하는 일(만들기, 복원, 지우기)
fn backup_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmBackup { backup } if backup == id => Some("being created"),
            TaskOp::BackupRestore { backup, .. } if backup == id => Some("being restored"),
            TaskOp::BackupDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        })
        .collect())
}

/// 노드 `id`가 온라인인지 확인한다. 아니면 409 `node_offline`.
fn check_online(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    match state.cluster.get::<NodeSpec>(id)? {
        Some(n) if n.status.online => Ok(()),
        n => Err(ApiError::conflict(
            "node_offline",
            format!(
                "node {:?} is offline",
                n.map_or_else(|| id.to_string(), |n| n.name)
            ),
        )),
    }
}

/// 노드 `id`의 이름(없으면 id)
fn node_name(state: &AppState, id: Uuid) -> Result<String, ApiError> {
    Ok(state
        .cluster
        .get::<NodeSpec>(id)?
        .map_or_else(|| id.to_string(), |n| n.name))
}

fn backup_busy(backup: &Backup, doing: &str) -> ApiError {
    ApiError::conflict(
        "backup_busy",
        format!(
            "backup {:?} is {doing}; try again when the task has finished",
            backup.name
        ),
    )
}

/// 풀에 백업이 남아 있으면 409 `pool_in_use`. 풀 지우기와 백업 표시 끄기가 쓴다.
pub(crate) fn check_no_backups(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    if backups.iter().any(|b| b.spec.pool == pool.name) {
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} holds backups", pool.name),
        ));
    }
    Ok(())
}

/// VM 디스크의 볼륨과 풀. 없으면 400, 만드는 중이면 409 `volume_not_ready`.
fn vm_disks(state: &AppState, vm: &Vm) -> Result<Vec<(Volume, StoragePool)>, ApiError> {
    let mut disks = Vec::with_capacity(vm.spec.disks.len());
    for disk in &vm.spec.disks {
        let volume = state
            .cluster
            .get_by_name::<VolumeSpec>(&disk.volume)?
            .ok_or_else(|| ApiError::bad_request(format!("volume {:?} not found", disk.volume)))?;
        if volume.status.phase != VolumePhase::Ready {
            return Err(ApiError::conflict(
                "volume_not_ready",
                format!("volume {:?} is still being created", disk.volume),
            ));
        }
        let pool = find_pool(state, &volume.spec.pool)?;
        disks.push((volume, pool));
    }
    Ok(disks)
}

/// 백업 Task를 실행할 노드. 실행 중인 VM은 그 노드, 꺼진 VM은 `spec.node`이고, 노드가 없으면 백업
/// 풀과 모든 디스크를 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(로컬 풀의 디스크는 파일이
/// 있는 노드에서만 읽힌다). 그런 노드가 없으면 409 `no_node`.
fn backup_node(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    disks: &[(Volume, StoragePool)],
) -> Result<Uuid, ApiError> {
    let placed = if is_stopped(vm) {
        vm.spec.node
    } else {
        vm.spec.node.or(vm.status.node)
    };
    if let Some(node) = placed {
        return Ok(node);
    }
    let reads = |node: &Node| {
        pool_usable_on(pool, node)
            && disks.iter().all(|(volume, p)| {
                pool_usable_on(p, node) && (is_shared(p) || volume.status.node == Some(node.id))
            })
    };
    first_node(state, reads)?.ok_or_else(|| {
        ApiError::conflict(
            "no_node",
            format!(
                "no online node can read the disks of VM {:?} and use storage pool {:?}",
                vm.name, pool.name
            ),
        )
    })
}

/// 모든 백업. `vm`(id나 이름)을 주면 그 VM의 백업만이다.
#[utoipa::path(get, path = "/backups", tag = "backups",
    params(BackupQuery),
    responses(
        (status = 200, body = List<Resource<BackupSpec, BackupStatus>>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn list_backups(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<BackupQuery>,
) -> Result<Json<List<Backup>>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    let (revision, mut items) = state.cluster.list::<BackupSpec>()?;
    if let Some(key) = q.vm {
        let id = match Uuid::try_parse(&key) {
            Ok(id) => Some(id),
            Err(_) => state.cluster.get_by_name::<VmSpec>(&key)?.map(|vm| vm.id),
        };
        // 이름이면 지금 그 이름인 VM의 백업과, 그 이름으로 남은 백업(지운 VM)을 함께 준다.
        let by_name = Uuid::try_parse(&key).is_err();
        items.retain(|b| Some(b.spec.vm) == id || (by_name && b.spec.vm_name == key));
    }
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    responses(
        (status = 200, body = Resource<BackupSpec, BackupStatus>),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn get_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Backup>, ApiError> {
    state.authorize(&principal, Action::Read, BackupSpec::KIND)?;
    Ok(Json(lookup::<BackupSpec>(&state.cluster, &key)?))
}

/// `pending` 백업을 만들고 그것을 채울 Task를 돌려준다(202). 풀은 백업 풀이어야 한다(400). 시작·정지
/// 중이면 409 `vm_busy`, 실행 중인데 스냅샷을 담지 못하는 디스크가 있으면 409 `vm_running`(스냅샷
/// 방식이 섞였으면 스냅샷처럼 409 `snapshot_unsupported`), Task를 돌릴 노드에서 백업 풀을 쓸 수 없으면
/// 409 `pool_not_on_node`·`pool_unavailable`, 돌릴 노드가 없으면 409 `no_node`, 이름이 겹치면 409
/// `name_taken`. 그사이 풀이 지워졌거나 백업 표시가 꺼졌으면 만든 백업을 지우고 409 `pool_unavailable`.
#[utoipa::path(post, path = "/vms/{key}/backups", tag = "backups",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = CreateBackupRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<CreateBackupRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    if let Some(name) = &req.name {
        check_name(name)?;
    }
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let pool = find_pool(&state, &req.pool)?;
    let node = check_backup(&state, &vm, &pool)?;
    let name = req
        .name
        .unwrap_or_else(|| default_backup_name(&vm.name, now()));
    let task = start_backup(&state, &vm, &pool, node, name, None).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// VM을 백업 풀 `pool`에 백업할 수 있는지 보고 백업 Task를 돌릴 노드를 돌려준다. 오류는
/// [`create_backup`]과 같다. 백업 요청 API와 백업 작업 스케줄러가 쓴다(권한 검사는 부르는 쪽).
pub(crate) fn check_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
) -> Result<Uuid, ApiError> {
    if !pool.spec.backups {
        return Err(ApiError::bad_request(format!(
            "storage pool {:?} does not store backups; turn on its backups setting",
            pool.name
        )));
    }
    check_not_busy(state, vm)?;
    let disks = vm_disks(state, vm)?;
    // 실행 중인 VM은 임시 스냅샷에서 읽는다.
    if !is_stopped(vm)
        && let Some((volume, _)) = disks
            .iter()
            .find(|(v, p)| !pool_kind_supports_snapshots(&p.spec.backend, v.spec.format))
    {
        return Err(ApiError::conflict(
            "vm_running",
            format!(
                "VM {:?} is running and its disk {:?} cannot be snapshotted; stop it to back it up",
                vm.name, volume.name
            ),
        ));
    }
    if !is_stopped(vm) {
        check_disks(state, vm)?;
    }
    let node = backup_node(state, vm, pool, &disks)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on the node holding VM {:?}",
            pool.name, vm.name
        )
    })?;
    Ok(node)
}

/// [`check_backup`]을 거친 VM의 `pending` 백업 `name`을 만들고 노드 `node`에서 그것을 채울 Task를
/// 돌려준다. 백업 작업이 만드는 백업은 `job`에 작업 id를 적는다.
pub(crate) async fn start_backup(
    state: &AppState,
    vm: &Vm,
    pool: &StoragePool,
    node: Uuid,
    name: String,
    job: Option<Uuid>,
) -> Result<Task, ApiError> {
    let backup: Backup = Resource::new(
        name,
        BackupSpec {
            vm: vm.id,
            vm_name: vm.name.clone(),
            pool: pool.name.clone(),
            vm_spec: vm.spec.clone(),
            job,
        },
    );
    // 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&backup).await?;
    // 풀 지우기·백업 표시 끄기는 백업이 없을 때만 된다. 그 확인과 엇갈렸으면 만든 백업을 거둔다.
    if !find_pool(state, &pool.name).is_ok_and(|p| p.spec.backups) {
        if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
            tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup of a removed pool");
        }
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} was deleted or stopped storing backups",
                pool.name
            ),
        ));
    }
    let op = TaskOp::VmBackup { backup: backup.id };
    match create_task(&state.cluster, op, vm.id, node).await {
        Ok(task) => Ok(task),
        Err(e) => {
            // 채울 Task가 없는 pending 백업은 이름만 차지한다.
            if let Err(d) = state.cluster.delete::<BackupSpec>(backup.id, None).await {
                tracing::warn!(backup = %backup.id, error = %d, "cannot remove backup without a task");
            }
            Err(e)
        }
    }
}

/// 백업 파일을 지울 노드: 파일을 쓴 노드(`status.node`, 공유가 아닌 풀)나 백업 풀을 쓸 수 있는 노드.
/// 그 노드가 오프라인이면 409 `node_offline`, 풀을 쓸 수 없으면 409 `pool_not_on_node`·`pool_unavailable`.
fn files_node(state: &AppState, backup: &Backup, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let Some(node) = backup.status.node else {
        return pool_node(state, pool);
    };
    check_online(state, node)?;
    check_pool_on(state, pool, node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding backup {:?}",
            pool.name,
            node_name(state, node).unwrap_or_else(|_| node.to_string()),
            backup.name
        )
    })?;
    Ok(node)
}

/// 백업 파일과 리소스를 지울 Task를 돌려준다(202). 실패한 백업도 지운다. 백업을 만들거나 되살리거나
/// 지우는 Task가 끝나지 않았으면 409 `backup_busy`. Task는 파일이 있는 노드(공유가 아닌 풀은 파일을
/// 쓴 노드, 공유 풀은 백업 풀을 쓸 수 있는 노드)에서 돈다. 풀이 없어졌으면 이 노드의 Task가 리소스만
/// 지운다. 파일을 지울 노드가 없으면 409(`node_offline`, `pool_unavailable` 등)이고, `force`면 리소스만
/// 바로 지우고 200이다(파일은 백업 풀에 남는다).
#[utoipa::path(delete, path = "/backups/{key}", tag = "backups",
    params(("key" = String, Path, description = "id or name"), DeleteBackupQuery),
    responses(
        (status = 202, body = Resource<TaskSpec, TaskStatus>),
        (status = 200, body = BackupForgotten, description = "force: only the resource was deleted"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn delete_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<DeleteBackupQuery>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    if q.force {
        check_backup_idle(&state, &backup)?;
        if let Err(e) = delete_node(&state, &backup)
            && e.status == StatusCode::CONFLICT
        {
            tracing::warn!(backup = %backup.name, pool = %backup.spec.pool, reason = %e.message, "deleting backup without its files");
            state.cluster.delete::<BackupSpec>(backup.id, None).await?;
            let message = format!(
                "backup {:?} deleted; its files are left in storage pool {:?} ({})",
                backup.name, backup.spec.pool, e.message
            );
            return Ok(Json(BackupForgotten { message }).into_response());
        }
    }
    let task = start_backup_delete(&state, &backup).await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

/// 백업을 만들거나 되살리거나 지우는 Task가 끝나지 않았으면 409 `backup_busy`
fn check_backup_idle(state: &AppState, backup: &Backup) -> Result<(), ApiError> {
    match backup_tasks(state, backup.id)?.first() {
        Some(doing) => Err(backup_busy(backup, doing)),
        None => Ok(()),
    }
}

/// 백업 지우기 Task를 돌릴 노드. 풀이 없어졌으면(파일도 함께 없어졌다) 이 노드다.
fn delete_node(state: &AppState, backup: &Backup) -> Result<Uuid, ApiError> {
    match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&backup.spec.pool)?
    {
        None => Ok(state.node),
        Some(pool) => files_node(state, backup, &pool),
    }
}

/// 백업 파일과 리소스를 지울 Task를 만든다. 오류는 `force` 없는 [`delete_backup`]과 같다. 백업 지우기
/// API와 백업 작업의 보존이 쓴다(권한 검사는 부르는 쪽).
pub(crate) async fn start_backup_delete(
    state: &AppState,
    backup: &Backup,
) -> Result<Task, ApiError> {
    check_backup_idle(state, backup)?;
    let node = delete_node(state, backup)?;
    create_task(&state.cluster, TaskOp::BackupDelete, backup.id, node).await
}

/// `ready` 백업을 새 VM `name`으로 되살릴 Task를 돌려준다(202). Task의 대상은 미리 정한 새 VM id다.
/// 백업의 VM 스펙이 쓰는 네트워크·보안 그룹이 지금 있어야 한다(400). 백업이 준비되지 않았으면 409
/// `backup_not_ready`, 지우는 중이면 409 `backup_busy`, 이름이 겹치면 409 `name_taken`. 노드는 `node`나
/// 백업 풀과 대상 풀을 함께 쓸 수 있는 온라인 노드 가운데 이름이 가장 앞선 노드다(없으면 409 `no_node`).
/// 공유가 아닌 풀의 백업은 파일을 쓴 노드(`status.node`)에서만 되살린다: `node`가 다르면 409
/// `backup_on_other_node`. 고른 노드가 오프라인이면 409 `node_offline`.
#[utoipa::path(post, path = "/backups/{key}/restore", tag = "backups",
    params(("key" = String, Path, description = "id or name")),
    request_body = RestoreRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn restore_backup(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, BackupSpec::KIND)?;
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&req.name)?;
    let backup = lookup::<BackupSpec>(&state.cluster, &key)?;
    // 이름이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&req.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", req.name),
        ));
    }
    if backup.status.phase != BackupPhase::Ready {
        return Err(ApiError::conflict(
            "backup_not_ready",
            format!("backup {:?} is not ready", backup.name),
        ));
    }
    if backup_tasks(&state, backup.id)?.contains(&"being deleted") {
        return Err(backup_busy(&backup, "being deleted"));
    }
    let spec = &backup.spec.vm_spec;
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    check_security_groups(&state, spec)?;
    let source = find_pool(&state, &backup.spec.pool)?;
    let target = find_pool(&state, &req.pool)?;
    let node = match (&req.node, backup.status.node) {
        (key, Some(stored)) => {
            if let Some(key) = key
                && lookup::<NodeSpec>(&state.cluster, key)?.id != stored
            {
                return Err(ApiError::conflict(
                    "backup_on_other_node",
                    format!(
                        "backup {:?} is stored on node {:?}",
                        backup.name,
                        node_name(&state, stored)?
                    ),
                ));
            }
            check_online(&state, stored)?;
            let name = node_name(&state, stored)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, stored, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, name
                    )
                })?;
            }
            stored
        }
        (Some(key), None) => {
            let node = lookup::<NodeSpec>(&state.cluster, key)?;
            check_online(&state, node.id)?;
            for pool in [&source, &target] {
                check_pool_on(&state, pool, node.id, || {
                    format!(
                        "storage pool {:?} is not available on node {:?}",
                        pool.name, node.name
                    )
                })?;
            }
            node.id
        }
        (None, None) => first_node(&state, |n| {
            pool_usable_on(&source, n) && pool_usable_on(&target, n)
        })?
        .ok_or_else(|| {
            ApiError::conflict(
                "no_node",
                format!(
                    "no online node can use both storage pools {:?} and {:?}",
                    source.name, target.name
                ),
            )
        })?,
    };
    let op = TaskOp::BackupRestore {
        backup: backup.id,
        name: req.name,
        pool: target.name,
    };
    let task = create_task(&state.cluster, op, Uuid::now_v7(), node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
```

`crates/opl-api/src/lib.rs`에서

```rust
pub mod auth;
mod backups;
```

을 다음으로 바꾼다:

```rust
pub mod auth;
mod backup_jobs;
mod backups;
```

`crates/opl-api/src/lib.rs`에서

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

을 다음으로 바꾼다:

```rust
        .routes(routes!(backups::restore_backup))
        .routes(routes!(r::list_backup_jobs, backup_jobs::create_backup_job))
        .routes(routes!(
            r::get_backup_job,
            backup_jobs::patch_backup_job,
            backup_jobs::delete_backup_job
        ))
        .routes(routes!(backup_jobs::run_backup_job))
        .routes(routes!(r::list_volumes, volumes::create_volume))
```

`crates/opl-api/src/resources.rs`에서

```rust
use opl_cluster::{Cluster, ClusterError, Typed};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
```

을 다음으로 바꾼다:

```rust
use opl_cluster::{Cluster, ClusterError, Typed};
use opl_types::backup_job::{BackupJobSpec, BackupJobStatus};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
```

`crates/opl-api/src/resources.rs`에서

```rust
    "security-groups"
);
```

을 다음으로 바꾼다:

```rust
    "security-groups"
);
list_and_get!(
    list_backup_jobs,
    get_backup_job,
    BackupJobSpec,
    BackupJobStatus,
    "/backup-jobs",
    "/backup-jobs/{key}",
    "backups"
);
```

`crates/opl-api/src/watch.rs`에서

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 9] = [
    Kind::Vm,
```

을 다음으로 바꾼다:

```rust
/// 구독할 수 있는 종류. 사용자와 토큰은 해시를 담고 있어 내보내지 않는다.
const WATCHABLE: [Kind; 10] = [
    Kind::Vm,
```

`crates/opl-api/src/watch.rs`에서

```rust
    Kind::Backup,
];
```

을 다음으로 바꾼다:

```rust
    Kind::Backup,
    Kind::BackupJob,
];
```

`crates/opl-api/src/watch.rs`에서

```rust
    #[test]
    fn security_groups_are_watchable() {
```

을 다음으로 바꾼다:

```rust
    #[test]
    fn backup_jobs_are_watchable() {
        let kinds = parse_kinds("backup-job").unwrap();
        assert!(kinds.contains(&Kind::BackupJob));
    }

    #[test]
    fn security_groups_are_watchable() {
```

`crates/opl-api/tests/backup_jobs.rs`를 만든다:

```rust
//! 백업 작업 API: 만들기·목록·읽기·바꾸기·지우기·지금 실행 요청의 검사와 기록되는 상태.
//! 스케줄러 없이 API가 남긴 리소스의 모양만 확인한다.

mod common;

use common::{Client, Env, vm_request};
use opl_types::backup::{Backup, BackupSpec};
use opl_types::backup_job::{BackupJobSpec, JobRun, JobSchedule, Weekday, next_run};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 디렉터리 풀을 저장소에 직접 만든다.
async fn dir_pool(env: &Env, name: &str, backups: bool) {
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: env.dir.path().join(name),
            },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 백업 풀 `backups`와 디스크 풀 `local`
async fn setup(env: &Env) {
    dir_pool(env, "backups", true).await;
    dir_pool(env, "local", false).await;
}

fn job(name: &str, spec: Value) -> Value {
    json!({ "name": name, "spec": spec })
}

/// `tier=prod` VM을 매일 03:00에 백업하고 최근 3개를 남기는 작업 스펙
fn nightly() -> Value {
    json!({
        "vms": { "labels": { "tier": "prod" } },
        "pool": "backups",
        "schedule": { "at": "03:00" },
        "keep": { "last": 3 },
    })
}

async fn create(admin: &Client, name: &str, spec: Value) -> Value {
    let (status, body) = admin.post("/backup-jobs", job(name, spec)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body
}

fn assert_error(got: &(StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(got.0, status, "{}", got.1);
    assert_eq!(got.1["code"], code, "{}", got.1);
}

fn schedule(at: &str, days: &[Weekday]) -> JobSchedule {
    JobSchedule {
        at: at.into(),
        days: days.to_vec(),
    }
}

#[tokio::test]
async fn create_records_the_job_with_its_next_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;

    let before = now();
    let body = create(&admin, "nightly", nightly()).await;
    let after = now();
    assert_eq!(body["spec"]["vms"], json!({ "labels": { "tier": "prod" } }));
    assert_eq!(
        body["spec"]["schedule"],
        json!({ "at": "03:00", "days": [] })
    );
    assert_eq!(
        body["spec"]["keep"],
        json!({ "last": 3, "daily": 0, "weekly": 0, "monthly": 0 })
    );
    // 적지 않으면 사용한다.
    assert_eq!(body["spec"]["enabled"], true);
    // 다음 실행은 만든 시각 뒤의 첫 03:00(UTC)이다.
    let next = body["status"]["next_run"].as_u64().unwrap();
    let sched = schedule("03:00", &[]);
    assert!(
        next_run(&sched, before) <= next && next <= next_run(&sched, after),
        "{body}"
    );
    assert_eq!(body["status"]["run"], Value::Null);
    assert_eq!(body["status"]["last_run"], Value::Null);

    let (status, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["id"], body["id"]);
    assert_eq!(got["status"]["next_run"], next);
    let (status, got) = admin
        .get(&format!("/backup-jobs/{}", body["id"].as_str().unwrap()))
        .await;
    assert_eq!(status, StatusCode::OK, "{got}");
    let (status, list) = admin.get("/backup-jobs").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert!(list["revision"].as_u64().is_some());

    // 모든 VM, 이름으로 고르기, 요일과 꺼 두기
    let all = json!({
        "vms": { "all": true },
        "pool": "backups",
        "schedule": { "at": "23:59", "days": ["sat", "sun"] },
        "enabled": false,
    });
    let body = create(&admin, "weekend", all).await;
    assert_eq!(body["spec"]["vms"], json!({ "all": true }));
    assert_eq!(body["spec"]["schedule"]["days"], json!(["sat", "sun"]));
    assert_eq!(body["spec"]["enabled"], false);
    let names = json!({
        "vms": { "names": ["web", "db"] },
        "pool": "backups",
        "schedule": { "at": "00:00" },
    });
    let body = create(&admin, "named", names).await;
    assert_eq!(body["spec"]["vms"], json!({ "names": ["web", "db"] }));
}

#[tokio::test]
async fn create_checks_the_pool_and_the_spec() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let with = |field: &str, value: Value| {
        let mut spec = nightly();
        spec[field] = value;
        job("nightly", spec)
    };

    // 풀은 있어야 하고 백업 풀이어야 한다.
    for (pool, msg) in [("nope", "not found"), ("local", "backups")] {
        let got = admin.post("/backup-jobs", with("pool", json!(pool))).await;
        assert_error(&got, StatusCode::BAD_REQUEST, "invalid_request");
        assert!(
            got.1["message"].as_str().unwrap().contains(msg),
            "{}",
            got.1
        );
    }
    let bad = [
        with("vms", json!({ "names": [] })),
        with("vms", json!({ "labels": {} })),
        with("vms", json!({ "all": false })),
        with("vms", json!({ "all": true, "names": ["web"] })),
        with("vms", json!({})),
        with("schedule", json!({ "at": "24:00" })),
        with("schedule", json!({ "at": "3:00" })),
        with("schedule", json!({ "at": "03:00", "days": ["mon", "mon"] })),
        with("schedule", json!({ "at": "03:00", "days": ["monday"] })),
        with("keep", json!({ "last": -1 })),
        job("0190f5a4-3e2b-7c1d-8e9f-a0b1c2d3e4f5", nightly()),
        job("Bad Name", nightly()),
    ];
    for req in bad {
        let got = admin.post("/backup-jobs", req.clone()).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{req} -> {}", got.1);
    }
    let got = admin
        .post(
            "/backup-jobs",
            with("schedule", json!({ "at": "03:00", "days": ["mon", "mon"] })),
        )
        .await;
    assert!(
        got.1["message"]
            .as_str()
            .unwrap()
            .contains("more than once"),
        "{}",
        got.1
    );
    let (_, list) = admin.get("/backup-jobs").await;
    assert_eq!(list["items"], json!([]));

    create(&admin, "nightly", nightly()).await;
    assert_error(
        &admin.post("/backup-jobs", job("nightly", nightly())).await,
        StatusCode::CONFLICT,
        "name_taken",
    );
}

#[tokio::test]
async fn patch_changes_given_fields_and_recomputes_the_next_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    dir_pool(&env, "more-backups", true).await;
    let created = create(&admin, "nightly", nightly()).await;
    let next = created["status"]["next_run"].as_u64().unwrap();

    // 보존만 바꾸면 다른 값과 다음 실행은 그대로다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "keep": { "daily": 7 } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["spec"]["keep"],
        json!({ "last": 0, "daily": 7, "weekly": 0, "monthly": 0 })
    );
    assert_eq!(body["spec"]["vms"], created["spec"]["vms"]);
    assert_eq!(body["spec"]["schedule"], created["spec"]["schedule"]);
    assert_eq!(body["status"]["next_run"], next);

    // 일정을 바꾸면 다음 실행을 다시 계산한다.
    let before = now();
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({
                "schedule": { "at": "12:30", "days": ["wed"] },
                "vms": { "all": true },
                "pool": "more-backups",
            }),
            None,
        )
        .await;
    let after = now();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["vms"], json!({ "all": true }));
    assert_eq!(body["spec"]["pool"], "more-backups");
    let next = body["status"]["next_run"].as_u64().unwrap();
    let sched = schedule("12:30", &[Weekday::Wed]);
    assert!(
        next_run(&sched, before) <= next && next <= next_run(&sched, after),
        "{body}"
    );
    let (_, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(got["status"]["next_run"], next);
    assert_eq!(got["revision"], body["revision"]);

    // 검사: 백업 풀이 아닌 풀, 잘못된 일정, 모르는 필드, 빈 요청
    for req in [
        json!({ "pool": "local" }),
        json!({ "schedule": { "at": "25:00" } }),
        json!({ "vms": { "names": [] } }),
        json!({ "name": "other" }),
        json!({}),
    ] {
        let got = admin.patch("/backup-jobs/nightly", req.clone(), None).await;
        assert_eq!(got.0, StatusCode::BAD_REQUEST, "{req} -> {}", got.1);
    }

    // If-Match가 맞지 않으면 412, 맞으면 쓴다.
    let rev = got["revision"].as_u64().unwrap();
    assert_error(
        &admin
            .patch(
                "/backup-jobs/nightly",
                json!({ "enabled": false }),
                Some(rev + 100),
            )
            .await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "enabled": false }),
            Some(rev),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["enabled"], false);
    assert_error(
        &admin
            .patch("/backup-jobs/nope", json!({ "enabled": true }), None)
            .await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn days_are_stored_in_week_order() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let mut spec = nightly();
    spec["schedule"] = json!({ "at": "03:00", "days": ["sun", "wed", "mon"] });
    let body = create(&admin, "nightly", spec).await;
    assert_eq!(
        body["spec"]["schedule"]["days"],
        json!(["mon", "wed", "sun"])
    );
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "03:00", "days": ["fri", "tue"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["schedule"]["days"], json!(["tue", "fri"]));
    // 순서만 다른 요일은 바꾼 것이 아니다.
    let rev = body["revision"].clone();
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "03:00", "days": ["tue", "fri"] } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], rev);
}

#[tokio::test]
async fn changing_the_schedule_keeps_a_requested_run() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    create(&admin, "nightly", nightly()).await;
    let (status, body) = admin.post("/backup-jobs/nightly/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let requested = body["status"]["next_run"].as_u64().unwrap();
    assert!(requested <= now(), "{body}");

    // 지금 실행 요청이 기다리는 동안 일정을 바꿔도 그 요청은 남는다.
    let (status, body) = admin
        .patch(
            "/backup-jobs/nightly",
            json!({ "schedule": { "at": "12:30" } }),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["spec"]["schedule"]["at"], "12:30");
    assert_eq!(body["status"]["next_run"], requested);
}

#[tokio::test]
async fn enabling_a_job_skips_runs_missed_while_disabled() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let mut spec = nightly();
    spec["enabled"] = json!(false);
    let created = create(&admin, "nightly", spec).await;
    // 꺼 둔 사이 다음 실행 시각이 지났다.
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let mut status = env
        .cluster
        .get::<BackupJobSpec>(id)
        .unwrap()
        .unwrap()
        .status;
    status.next_run = Some(1);
    env.cluster
        .update_status::<BackupJobSpec>(id, &status)
        .await
        .unwrap();

    let before = now();
    let (code, body) = admin
        .patch("/backup-jobs/nightly", json!({ "enabled": true }), None)
        .await;
    assert_eq!(code, StatusCode::OK, "{body}");
    let next = body["status"]["next_run"].as_u64().unwrap();
    assert!(next >= next_run(&schedule("03:00", &[]), before), "{body}");
}

#[tokio::test]
async fn run_asks_for_a_run_now_unless_one_is_going() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let created = create(&admin, "nightly", nightly()).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let before = now();
    let (status, body) = admin.post("/backup-jobs/nightly/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let next = body["status"]["next_run"].as_u64().unwrap();
    assert!(before <= next && next <= now(), "{body}");
    let (_, got) = admin.get("/backup-jobs/nightly").await;
    assert_eq!(got["status"]["next_run"], next);

    // 실행이 진행 중이면 409 job_running
    let mut status = env
        .cluster
        .get::<BackupJobSpec>(id)
        .unwrap()
        .unwrap()
        .status;
    status.run = Some(JobRun {
        started_at: next,
        queue: vec![],
        backups: vec![],
    });
    env.cluster
        .update_status::<BackupJobSpec>(id, &status)
        .await
        .unwrap();
    assert_error(
        &admin.post("/backup-jobs/nightly/run", Value::Null).await,
        StatusCode::CONFLICT,
        "job_running",
    );

    // 꺼 둔 작업은 실행하지 않는다.
    let mut spec = nightly();
    spec["enabled"] = json!(false);
    create(&admin, "off", spec).await;
    assert_error(
        &admin.post("/backup-jobs/off/run", Value::Null).await,
        StatusCode::CONFLICT,
        "job_disabled",
    );
    assert_error(
        &admin.post("/backup-jobs/nope/run", Value::Null).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

#[tokio::test]
async fn deleting_a_job_keeps_its_backups() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let created = create(&admin, "nightly", nightly()).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let vm_spec: VmSpec = serde_json::from_value(vm_request("web")["spec"].clone()).unwrap();
    let backup: Backup = Resource::new(
        "web-20261104-030000",
        BackupSpec {
            vm: Uuid::now_v7(),
            vm_name: "web".into(),
            pool: "backups".into(),
            vm_spec,
            job: Some(id),
        },
    );
    env.cluster.create(&backup).await.unwrap();

    // If-Match가 맞지 않으면 지우지 않는다.
    let rev = created["revision"].as_u64().unwrap();
    let r = admin
        .http
        .delete(format!("{}/api/v1/backup-jobs/nightly", admin.base))
        .bearer_auth(admin.token.as_ref().unwrap())
        .header("if-match", (rev + 100).to_string());
    assert_error(
        &admin.send(r).await,
        StatusCode::PRECONDITION_FAILED,
        "revision_mismatch",
    );

    let (status, body) = admin.delete("/backup-jobs/nightly").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        admin.get("/backup-jobs/nightly").await.0,
        StatusCode::NOT_FOUND
    );
    let (status, got) = admin.get(&format!("/backups/{}", backup.id)).await;
    assert_eq!(status, StatusCode::OK, "{got}");
    assert_eq!(got["spec"]["job"], id.to_string());
}

#[tokio::test]
async fn viewers_read_jobs_and_operators_manage_them() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env).await;
    let viewer = env.user("viewer", "viewer").await;
    let operator = env.user("operator", "operator").await;
    create(&admin, "nightly", nightly()).await;

    assert_eq!(viewer.get("/backup-jobs").await.0, StatusCode::OK);
    assert_eq!(viewer.get("/backup-jobs/nightly").await.0, StatusCode::OK);
    for got in [
        viewer.post("/backup-jobs", job("other", nightly())).await,
        viewer
            .patch("/backup-jobs/nightly", json!({ "enabled": false }), None)
            .await,
        viewer.post("/backup-jobs/nightly/run", Value::Null).await,
        viewer.delete("/backup-jobs/nightly").await,
    ] {
        assert_error(&got, StatusCode::FORBIDDEN, "forbidden");
    }

    let (status, body) = operator.post("/backup-jobs", job("other", nightly())).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = operator
        .patch("/backup-jobs/other", json!({ "keep": { "last": 1 } }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = operator.post("/backup-jobs/other/run", Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = operator.delete("/backup-jobs/other").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}
```

`crates/opl-api/tests/openapi.rs`에서

```rust
        "/backups/{key}/restore",
        "/watch",
```

을 다음으로 바꾼다:

```rust
        "/backups/{key}/restore",
        "/backup-jobs",
        "/backup-jobs/{key}",
        "/backup-jobs/{key}/run",
        "/watch",
```

`crates/opl-api/tests/openapi.rs`에서

```rust
    assert!(doc["components"]["schemas"]["Resource_BackupSpec_BackupStatus"].is_object());
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
```

을 다음으로 바꾼다:

```rust
    assert!(doc["components"]["schemas"]["Resource_BackupSpec_BackupStatus"].is_object());
    assert!(doc["components"]["schemas"]["Resource_BackupJobSpec_BackupJobStatus"].is_object());
    // 백업 작업의 VM 고르기는 세 객체 중 하나다.
    let selector = &doc["components"]["schemas"]["VmSelector"]["oneOf"];
    assert_eq!(selector.as_array().map(Vec::len), Some(3), "{selector}");
    // 마이그레이션 상태는 없으면 빼고 쓰므로 선택 필드다(null도 기본값도 아니다).
```

- [ ] **Step 2: 생성 파일과 준비**

Run: `pnpm -C web gen:api`
Expected: `web/src/api/schema.d.ts`가 바뀐다(커밋한다)

- [ ] **Step 3: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1001개, 실패 0

Run: `cargo test -p opl-api --test backup_jobs --test backups --test openapi`
Expected: PASS

Run: `pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 368개

- [ ] **Step 4: 커밋**

```bash
git add crates web
git commit -m "feat(opl-api): 백업 작업 API — /backup-jobs 만들기·목록·읽기·PATCH·지우기·지금 실행, watch 종류 backup-job, 백업 만들기·지우기 Task 만들기를 함수로 빼기"
```

---

### Task 3: 백업 작업 스케줄러

`BackupJobScheduler`(리더만, opld `[backup] tick_secs`(기본 60)·`max_concurrent`(노드당 끝나지 않은 백업 Task, 기본 2)): 사용 중이고 `next_run`이 지난 작업은 대상 VM으로 실행을 시작하고(놓친 실행은 한 번만, 없는 이름은 오류), VM마다 이번 실행에서 이미 만든 백업이 있으면 받아들이고(상태 쓰기 실패·리더 바뀜 뒤 중복을 막음), 없으면 동시 제한 안에서 `job`이 붙은 백업을 요청한다(이름이 겹치면 63자 안에서 `-N`). 큐가 비고 이번 백업이 모두 끝나면(Task가 끝났는데 pending인 백업은 실패로 셈) `last_run`을 쓰고, 보존 규칙으로 VM마다 지울 백업과 더 새 ready가 있는 실패한 백업을 지운다. 끄면 남은 VM은 비운다.

**Files:**
- Create: `crates/opl-api/tests/backup_job_runs.rs`
- Modify: `crates/opl-api/src/backup_jobs.rs`
- Modify: `crates/opl-api/src/backups.rs`
- Modify: `crates/opl-api/src/lib.rs`
- Modify: `crates/opld/src/config.rs`
- Modify: `crates/opld/src/member.rs`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/opl-api/src/backup_jobs.rs`에서

```rust
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.

use std::sync::Arc;
```

을 다음으로 바꾼다:

```rust
//! 쓰기는 백업과 같은 권한(operator 이상)이다. 작업을 지워도 그 작업이 만든 백업은 남는다.
//!
//! [`BackupJobScheduler`]는 리더에서 `tick`마다 작업을 이름 순으로 보며 실행을 시작하고, 대상 VM을
//! 하나씩 꺼내 백업을 요청하고(노드당 동시 백업 Task 수 제한), 실행이 끝나면 결과를 적고 보존 규칙대로
//! 지운다. 진행 상태는 작업의 `status.run`에 있으므로 리더가 바뀌어도 이어 간다. 리더가 바뀌는 순간
//! 두 스케줄러가 잠깐 겹치거나 상태 쓰기가 실패해도, 이번 실행이 이미 만든 VM의 백업은 다시 만들지 않고
//! 받아들인다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
use opl_cluster::types::Rejection;
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobSchedule, Keep, VmSelector, next_run,
};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use serde::Deserialize;
use utoipa::ToSchema;
```

을 다음으로 바꾼다:

```rust
use opl_cluster::types::Rejection;
use opl_types::backup::{Backup, BackupPhase, BackupSpec, default_backup_name};
use opl_types::backup_job::{
    BackupJob, BackupJobSpec, BackupJobStatus, JobError, JobRun, JobRunResult, JobSchedule, Keep,
    VmSelector, next_run, prune,
};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::task::{Task, TaskOp, TaskSpec};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
```

을 다음으로 바꾼다:

```rust
use crate::auth::{Action, Principal};
use crate::backups::{check_backup, now, start_backup, start_backup_delete};
use crate::error::{ApiError, Json};
```

`crates/opl-api/src/backup_jobs.rs`에서

```rust
    Ok(Json(job))
}
```

을 다음으로 바꾼다:

```rust
    Ok(Json(job))
}

#[derive(Debug, Clone)]
pub struct BackupJobSchedulerConfig {
    /// 작업을 확인하는 간격
    pub tick: Duration,
    /// 노드마다 동시에 도는 백업 Task의 최대 수. 손으로 요청한 백업도 센다.
    pub max_concurrent: usize,
}

impl Default for BackupJobSchedulerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            max_concurrent: 2,
        }
    }
}

/// 백업 작업 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 일한다.
pub struct BackupJobScheduler {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl BackupJobScheduler {
    pub fn spawn(state: Arc<AppState>, config: BackupJobSchedulerConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(
    state: Arc<AppState>,
    config: BackupJobSchedulerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 선거 중에는 기록이 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config) => {}
        }
    }
}

async fn tick(state: &AppState, config: &BackupJobSchedulerConfig) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 목록을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, mut jobs)) = cluster.list::<BackupJobSpec>() else {
        return;
    };
    jobs.sort_by(|a, b| a.name.cmp(&b.name));
    let now = now();
    for job in jobs {
        // 쓰기가 엇갈렸거나(revision) 저장소 오류면 다음 tick에 다시 본다.
        if let Err(e) = advance(state, config, job.clone(), now).await {
            tracing::warn!(job = %job.name, error = %e.message, "cannot advance backup job");
        }
    }
}

/// 작업 하나를 한 걸음 나아가게 한다: 때가 되면 실행을 시작하고, 큐에서 VM을 꺼내 백업을 요청하고,
/// 큐가 비고 이번 실행의 백업이 모두 끝났으면 실행을 끝내고 보존 규칙대로 지운다.
async fn advance(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    mut job: BackupJob,
    now: u64,
) -> Result<(), ApiError> {
    let cluster = &state.cluster;
    if job.status.run.is_none() {
        if !job.spec.enabled || job.status.next_run.is_none_or(|t| t > now) {
            return Ok(());
        }
        let (_, mut vms) = cluster.list::<VmSpec>()?;
        vms.sort_by(|a, b| a.name.cmp(&b.name));
        let queue: Vec<Uuid> = vms
            .iter()
            .filter(|vm| job.spec.vms.matches(vm))
            .map(|vm| vm.id)
            .collect();
        // 이름으로 골랐는데 그 VM이 없으면 오류로 남긴다.
        let missing = match &job.spec.vms {
            VmSelector::Names(names) => names
                .iter()
                .filter(|name| !vms.iter().any(|vm| vm.name == **name))
                .map(|name| JobError {
                    vm: name.clone(),
                    error: format!("no VM named {name:?}"),
                })
                .collect(),
            VmSelector::All | VmSelector::Labels(_) => Vec::new(),
        };
        // 놓친 실행이 여럿이어도 한 번만 하고 다음 실행은 지금부터 센다.
        let status = BackupJobStatus {
            next_run: Some(next_run(&job.spec.schedule, now)),
            run: Some(JobRun {
                started_at: now,
                queue,
                backups: Vec::new(),
            }),
            last_run: job.status.last_run.clone(),
            errors: missing,
        };
        job.revision = cluster
            .update_status_checked::<BackupJobSpec>(job.id, job.revision, &status)
            .await?;
        job.status = status;
        tracing::info!(job = %job.name, "backup job run started");
    }

    let queue = job
        .status
        .run
        .as_ref()
        .map_or(Vec::new(), |r| r.queue.clone());
    for vm in queue {
        // 실행 도중 꺼졌으면 새 VM은 꺼내지 않는다.
        if !job.spec.enabled {
            break;
        }
        let outcome = dispatch(state, config, &job, vm).await?;
        if outcome == Dispatch::Wait {
            continue;
        }
        job = update_status(state, job.id, |job| take(job, vm, &outcome)).await?;
    }
    if !job.spec.enabled && job.status.run.as_ref().is_some_and(|r| !r.queue.is_empty()) {
        job = update_status(state, job.id, |job| {
            let mut status = job.status.clone();
            if let Some(run) = &mut status.run {
                run.queue.clear();
            }
            Ok(status)
        })
        .await?;
    }

    // Task를 먼저 읽는다: 백업 Task는 백업 상태를 쓴 뒤에 Task를 끝내므로, 이 순서면 끝난 Task의
    // 백업이 pending으로 보이지 않는다.
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if finish(&job.status, &backups, &tasks, now).is_none() {
        return Ok(());
    }
    let job = update_status(state, job.id, |job| {
        finish(&job.status, &backups, &tasks, now)
            .ok_or_else(|| ApiError::conflict("job_running", "the backup job run changed"))
    })
    .await?;
    if let Some(last) = &job.status.last_run {
        tracing::info!(job = %job.name, ok = last.ok, failed = last.failed, "backup job run finished");
    }
    for id in retention(job.id, &job.spec.keep, &backups) {
        let Some(backup) = backups.iter().find(|b| b.id == id) else {
            continue;
        };
        // 지우지 못하면(이미 지우는 중 등) 다음 실행이 끝날 때 다시 고른다.
        match start_backup_delete(state, backup).await {
            Ok(task) => {
                tracing::info!(job = %job.name, backup = %backup.name, task = %task.id, "pruning backup")
            }
            Err(e) => {
                tracing::info!(job = %job.name, backup = %backup.name, error = %e.message, "cannot prune backup")
            }
        }
    }
    Ok(())
}

/// 큐에서 꺼낸 VM 하나에 한 일
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dispatch {
    /// 노드가 이미 백업 Task를 다 돌리고 있다. 큐에 남긴다.
    Wait,
    /// VM이 없어졌다.
    Gone,
    /// 이 백업을 만들었다(또는 이번 실행이 이미 만들었다).
    Started(Uuid),
    /// 백업하지 못했다.
    Refused(JobError),
}

/// 큐의 VM `vm`의 백업을 요청한다. 요청 검사는 백업 API와 같다(거절은 [`Dispatch::Refused`]).
/// 이번 실행이 이 VM의 백업을 이미 만들었으면(상태에 적지 못한 채 리더가 바뀌었거나 쓰기가 실패했다)
/// 그 백업을 받아들인다.
async fn dispatch(
    state: &AppState,
    config: &BackupJobSchedulerConfig,
    job: &BackupJob,
    vm: Uuid,
) -> Result<Dispatch, ApiError> {
    let cluster = &state.cluster;
    let Some(vm) = cluster.get::<VmSpec>(vm)? else {
        return Ok(Dispatch::Gone);
    };
    let (_, backups) = cluster.list::<BackupSpec>()?;
    if let Some(made) = backups
        .iter()
        .find(|b| b.spec.vm == vm.id && of_this_run(job, b))
    {
        return Ok(Dispatch::Started(made.id));
    }
    let refused = |error: String| {
        Ok(Dispatch::Refused(JobError {
            vm: vm.name.clone(),
            error,
        }))
    };
    let (_, tasks) = cluster.list::<TaskSpec>()?;
    let backing_up =
        |t: &&Task| matches!(t.spec.op, TaskOp::VmBackup { .. }) && !t.status.state.is_finished();
    if tasks
        .iter()
        .filter(backing_up)
        .any(|t| t.spec.target == vm.id)
    {
        return refused("a backup of this VM is already running".into());
    }
    let Some(pool) = cluster.get_by_name::<StoragePoolSpec>(&job.spec.pool)? else {
        return refused(format!("storage pool {:?} not found", job.spec.pool));
    };
    let node = match check_backup(state, &vm, &pool) {
        Ok(node) => node,
        Err(e) if e.status.is_client_error() => return refused(e.message),
        Err(e) => return Err(e),
    };
    let running = tasks
        .iter()
        .filter(backing_up)
        .filter(|t| t.spec.node == node)
        .count();
    if running >= config.max_concurrent {
        return Ok(Dispatch::Wait);
    }
    let name = backup_name(&vm.name, now(), &backups);
    match start_backup(state, &vm, &pool, node, name, Some(job.id)).await {
        Ok(task) => match task.spec.op {
            TaskOp::VmBackup { backup } => Ok(Dispatch::Started(backup)),
            _ => unreachable!("start_backup makes a backup task"),
        },
        Err(e) if e.status.is_client_error() => refused(e.message),
        Err(e) => Err(e),
    }
}

/// 백업 `backup`이 작업 `job`의 진행 중인 실행이 만든 것인지. 백업 id(v7)의 시각(초)이 실행을 시작한
/// 시각 이후이고 마지막으로 끝난 실행의 끝난 시각보다 뒤여야 한다(앞 실행의 백업은 그 실행이 끝나기 전에
/// 만들었다).
fn of_this_run(job: &BackupJob, backup: &Backup) -> bool {
    let Some(run) = &job.status.run else {
        return false;
    };
    let Some(made) = backup.id.get_timestamp().map(|t| t.to_unix().0) else {
        return false;
    };
    backup.spec.job == Some(job.id)
        && made >= run.started_at
        && job
            .status
            .last_run
            .as_ref()
            .is_none_or(|l| made > l.finished_at)
}

/// 실행의 큐에서 `vm`을 빼고 한 일을 적은 상태
fn take(job: &BackupJob, vm: Uuid, outcome: &Dispatch) -> Result<BackupJobStatus, ApiError> {
    let mut status = job.status.clone();
    let Some(run) = &mut status.run else {
        return Err(ApiError::conflict(
            "job_not_running",
            "the backup job run ended",
        ));
    };
    run.queue.retain(|id| *id != vm);
    match outcome {
        Dispatch::Started(backup) if !run.backups.contains(backup) => run.backups.push(*backup),
        Dispatch::Refused(error) => status.errors.push(error.clone()),
        Dispatch::Started(_) | Dispatch::Wait | Dispatch::Gone => {}
    }
    Ok(status)
}

/// 기본 백업 이름(`{VM 이름}-{yyyymmdd-HHMMSS}`). 같은 이름의 백업이 있으면 `-2`, `-3`…을 붙인다.
/// 번호를 붙이면 63자를 넘지 않게 VM 이름 부분을 번호 길이만큼 줄인다.
fn backup_name(vm: &str, now: u64, backups: &[Backup]) -> String {
    let base = default_backup_name(vm, now);
    let taken = |name: &str| backups.iter().any(|b| b.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| {
            let suffix = format!("-{n}");
            // 기본 이름은 VM 이름의 앞 47자를 쓴다.
            let short = &vm[..vm.len().min(47 - suffix.len())];
            format!("{}{suffix}", default_backup_name(short, now))
        })
        .find(|name| !taken(name))
        .unwrap()
}

/// 끝난 실행을 적은 상태. 실행이 없거나, 큐가 남았거나, 이번 실행의 백업이 아직 만드는 중이면 없다.
/// 성공은 `ready` 백업, 실패는 `failed` 백업과 백업하지 못한 VM이다(없어진 백업은 세지 않는다).
/// 채우는 끝나지 않은 Task가 없는 `pending` 백업(죽은 노드의 Task를 실패 처리했다)도 실패다.
/// 실패한 백업의 오류는 `errors`에 더한다.
fn finish(
    status: &BackupJobStatus,
    backups: &[Backup],
    tasks: &[Task],
    now: u64,
) -> Option<BackupJobStatus> {
    let run = status.run.as_ref()?;
    if !run.queue.is_empty() {
        return None;
    }
    let mine: Vec<&Backup> = run
        .backups
        .iter()
        .filter_map(|id| backups.iter().find(|b| b.id == *id))
        .collect();
    let filling = |b: &Backup| {
        tasks.iter().any(|t| {
            matches!(t.spec.op, TaskOp::VmBackup { backup } if backup == b.id)
                && !t.status.state.is_finished()
        })
    };
    let (pending, mine): (Vec<&Backup>, Vec<&Backup>) = mine
        .into_iter()
        .partition(|b| b.status.phase == BackupPhase::Pending);
    if pending.iter().any(|b| filling(b)) {
        return None;
    }
    let ok = mine
        .iter()
        .filter(|b| b.status.phase == BackupPhase::Ready)
        .count() as u32;
    let mut errors = status.errors.clone();
    errors.extend(
        mine.iter()
            .filter(|b| b.status.phase == BackupPhase::Failed)
            .map(|b| JobError {
                vm: b.spec.vm_name.clone(),
                error: b
                    .status
                    .error
                    .clone()
                    .unwrap_or_else(|| "backup failed".into()),
            }),
    );
    errors.extend(pending.iter().map(|b| JobError {
        vm: b.spec.vm_name.clone(),
        error: "backup task ended without finishing the backup".into(),
    }));
    Some(BackupJobStatus {
        next_run: status.next_run,
        run: None,
        last_run: Some(JobRunResult {
            started_at: run.started_at,
            finished_at: now,
            ok,
            failed: errors.len() as u32,
        }),
        errors,
    })
}

/// 작업 `job`의 백업 가운데 지울 것: VM마다 `ready` 백업은 보존 규칙(`prune`)에 맞지 않는 것,
/// `failed` 백업은 같은 VM의 더 새(id가 큰) `ready` 백업이 있는 것. 다른 작업과 손으로 만든 백업은
/// 건드리지 않는다.
fn retention(job: Uuid, keep: &Keep, backups: &[Backup]) -> Vec<Uuid> {
    let mut by_vm: BTreeMap<Uuid, Vec<&Backup>> = BTreeMap::new();
    for b in backups.iter().filter(|b| b.spec.job == Some(job)) {
        by_vm.entry(b.spec.vm).or_default().push(b);
    }
    let mut out = Vec::new();
    for mine in by_vm.values() {
        let ready: Vec<(Uuid, u64)> = mine
            .iter()
            .filter(|b| b.status.phase == BackupPhase::Ready)
            .filter_map(|b| Some((b.id, b.status.created_at?)))
            .collect();
        out.extend(prune(&ready, keep));
        if let Some(newest) = ready.iter().map(|b| b.0).max() {
            out.extend(
                mine.iter()
                    .filter(|b| b.status.phase == BackupPhase::Failed && b.id < newest)
                    .map(|b| b.id),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use opl_types::backup::BackupStatus;
    use opl_types::backup_job::JobSchedule;
    use opl_types::resource::validate_name;
    use opl_types::task::TaskState;
    use opl_types::vm::{CpuSpec, MachineType, PowerState};

    use super::*;

    /// 2026-10-27 05:07:00 UTC
    const NOW: u64 = 1_793_077_620;
    const DAY: u64 = 86400;

    fn vm_spec() -> VmSpec {
        VmSpec {
            cpu: CpuSpec {
                sockets: 1,
                cores: 1,
                model: Default::default(),
            },
            memory_mib: 256,
            firmware: Default::default(),
            machine: MachineType::Virt,
            disks: vec![],
            nics: vec![],
            power: PowerState::Stopped,
            restart: Default::default(),
            node: None,
            snapshot_schedule: None,
            ha: false,
        }
    }

    /// VM `vm`의 백업. `created_at`이 있으면 `phase`로 끝났다.
    fn backup(
        name: &str,
        vm: Uuid,
        job: Option<Uuid>,
        phase: BackupPhase,
        created_at: Option<u64>,
    ) -> Backup {
        let mut b: Backup = Resource::new(
            name,
            BackupSpec {
                vm,
                vm_name: format!("vm-{}", &vm.simple().to_string()[..4]),
                pool: "bk".into(),
                vm_spec: vm_spec(),
                job,
            },
        );
        b.status = BackupStatus {
            phase,
            created_at,
            error: (phase == BackupPhase::Failed).then(|| format!("{name} broke")),
            ..Default::default()
        };
        b
    }

    fn running(queue: Vec<Uuid>, backups: Vec<Uuid>, errors: Vec<JobError>) -> BackupJobStatus {
        BackupJobStatus {
            next_run: Some(NOW + DAY),
            run: Some(JobRun {
                started_at: NOW - 60,
                queue,
                backups,
            }),
            last_run: None,
            errors,
        }
    }

    /// 백업 `backup`을 채우는 Task
    fn task(backup: Uuid, state: TaskState) -> Task {
        let mut t: Task = Resource::new(
            "t",
            TaskSpec {
                op: TaskOp::VmBackup { backup },
                target: Uuid::now_v7(),
                node: Uuid::now_v7(),
            },
        );
        t.status.state = state;
        t
    }

    fn refused(vm: &str) -> JobError {
        JobError {
            vm: vm.into(),
            error: "no".into(),
        }
    }

    #[test]
    fn names_follow_the_default_with_a_number_when_taken() {
        let vm = Uuid::now_v7();
        assert_eq!(backup_name("web", NOW, &[]), "web-20261027-050700");
        let taken = [
            backup("web-20261027-050700", vm, None, BackupPhase::Ready, Some(1)),
            backup(
                "web-20261027-050700-2",
                vm,
                None,
                BackupPhase::Ready,
                Some(2),
            ),
        ];
        assert_eq!(backup_name("web", NOW, &taken), "web-20261027-050700-3");
        assert_eq!(backup_name("db", NOW, &taken), "db-20261027-050700");

        // 63자 이름 둘의 앞부분이 같으면 번호를 붙이되 63자를 넘지 않게 앞부분을 줄인다.
        let a = format!("{}a", "x".repeat(62));
        let b = format!("{}b", "x".repeat(62));
        let first = backup_name(&a, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&b, NOW, &taken);
        assert_ne!(first, second);
        for name in [&first, &second] {
            assert!(name.len() <= 63, "{name}");
            assert_eq!(validate_name(name), Ok(()), "{name}");
        }
        assert!(second.ends_with("-2"), "{second}");
        // 줄인 끝이 `-`이면 그것도 뗀다.
        let dashed = format!("{}-{}", "y".repeat(44), "z".repeat(18));
        let first = backup_name(&dashed, NOW, &[]);
        let taken = [backup(&first, vm, None, BackupPhase::Ready, Some(1))];
        let second = backup_name(&dashed, NOW, &taken);
        assert_eq!(second, format!("{}-20261027-050700-2", "y".repeat(44)));
    }

    #[test]
    fn a_run_finishes_when_its_queue_is_empty_and_its_backups_are_done() {
        let vm = Uuid::now_v7();
        let ready = backup("a", vm, None, BackupPhase::Ready, Some(NOW - 10));
        let failed = backup("b", vm, None, BackupPhase::Failed, None);
        let pending = backup("c", vm, None, BackupPhase::Pending, None);
        let all = [ready.clone(), failed.clone(), pending.clone()];
        let filling = [task(pending.id, TaskState::Running)];

        // 실행이 없거나, 큐가 남았거나, 만드는 중인 백업이 있으면 끝나지 않았다.
        assert_eq!(finish(&BackupJobStatus::default(), &all, &[], NOW), None);
        let queued = running(vec![vm], vec![ready.id], vec![]);
        assert_eq!(finish(&queued, &all, &[], NOW), None);
        let waiting = running(vec![], vec![ready.id, pending.id], vec![]);
        assert_eq!(finish(&waiting, &all, &filling, NOW), None);

        // 채우던 Task가 끝났는데(실패 처리 등) pending으로 남은 백업은 실패다.
        for tasks in [vec![], vec![task(pending.id, TaskState::Failed)]] {
            let done = finish(&waiting, &all, &tasks, NOW).unwrap();
            let last = done.last_run.unwrap();
            assert_eq!((last.ok, last.failed), (1, 1));
            assert_eq!(
                done.errors,
                [JobError {
                    vm: pending.spec.vm_name.clone(),
                    error: "backup task ended without finishing the backup".into(),
                }]
            );
        }

        // ready는 성공, failed와 백업하지 못한 VM은 실패다. 없어진 백업은 세지 않는다.
        let gone = Uuid::now_v7();
        let status = running(vec![], vec![ready.id, failed.id, gone], vec![refused("db")]);
        let done = finish(&status, &all, &filling, NOW).unwrap();
        assert_eq!(done.run, None);
        assert_eq!(done.next_run, Some(NOW + DAY));
        assert_eq!(
            done.last_run,
            Some(JobRunResult {
                started_at: NOW - 60,
                finished_at: NOW,
                ok: 1,
                failed: 2,
            })
        );
        assert_eq!(
            done.errors,
            [
                refused("db"),
                JobError {
                    vm: failed.spec.vm_name.clone(),
                    error: "b broke".into(),
                }
            ]
        );
        // 백업 없이 끝난 실행
        let empty = finish(&running(vec![], vec![], vec![]), &[], &[], NOW).unwrap();
        assert_eq!(empty.last_run.unwrap().ok, 0);
    }

    #[test]
    fn taking_a_vm_records_what_happened() {
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
            },
        );
        job.status = running(vec![a, b], vec![], vec![]);
        let backup = Uuid::now_v7();
        let status = take(&job, a, &Dispatch::Started(backup)).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [b]);
        assert_eq!(status.run.as_ref().unwrap().backups, [backup]);
        let status = take(&job, b, &Dispatch::Refused(refused("db"))).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, [refused("db")]);
        let status = take(&job, b, &Dispatch::Gone).unwrap();
        assert_eq!(status.run.as_ref().unwrap().queue, [a]);
        assert_eq!(status.errors, []);
        job.status.run = None;
        assert!(take(&job, a, &Dispatch::Gone).is_err());
    }

    #[test]
    fn a_backup_belongs_to_the_run_made_after_it_started() {
        let vm = Uuid::now_v7();
        let mut job: BackupJob = Resource::new(
            "nightly",
            BackupJobSpec {
                vms: VmSelector::All,
                pool: "bk".into(),
                schedule: JobSchedule {
                    at: "03:00".into(),
                    days: vec![],
                },
                keep: Keep::default(),
                enabled: true,
            },
        );
        // 실행은 NOW - 60에 시작했다.
        job.status = running(vec![vm], vec![], vec![]);
        let made_at = |at: u64, job: Option<Uuid>| {
            let mut b = backup("b", vm, job, BackupPhase::Pending, None);
            b.id = Uuid::new_v7(uuid::Timestamp::from_unix(uuid::NoContext, at, 0));
            b
        };
        assert!(of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW, Some(job.id))));
        assert!(!of_this_run(&job, &made_at(NOW - 61, Some(job.id))));
        // 다른 작업과 손으로 만든 백업
        assert!(!of_this_run(&job, &made_at(NOW, Some(Uuid::now_v7()))));
        assert!(!of_this_run(&job, &made_at(NOW, None)));
        // 앞 실행이 실행을 시작한 그 초에 끝났으면 그 초의 백업은 앞 실행의 것이다.
        job.status.last_run = Some(JobRunResult {
            started_at: NOW - 120,
            finished_at: NOW - 60,
            ok: 1,
            failed: 0,
        });
        assert!(!of_this_run(&job, &made_at(NOW - 60, Some(job.id))));
        assert!(of_this_run(&job, &made_at(NOW - 59, Some(job.id))));
        // 실행이 없으면 아무것도 아니다.
        job.status.run = None;
        assert!(!of_this_run(&job, &made_at(NOW, Some(job.id))));
    }

    #[test]
    fn retention_prunes_ready_backups_per_vm_and_failed_ones_before_a_success() {
        let job = Uuid::now_v7();
        let (web, db) = (Uuid::now_v7(), Uuid::now_v7());
        let keep = Keep {
            last: 1,
            ..Keep::default()
        };
        // id가 만든 순서다(v7).
        let with_id = |mut b: Backup, n: u128| {
            b.id = Uuid::from_u128(n);
            b
        };
        let r = |vm, at, n| with_id(backup("r", vm, Some(job), BackupPhase::Ready, Some(at)), n);
        let f = |vm, n| with_id(backup("f", vm, Some(job), BackupPhase::Failed, None), n);
        // 만든 순서: web 실패 → web 성공 둘 → web 실패, db 성공 둘
        let web_failed_old = f(web, 1);
        let web_old = r(web, NOW - 2 * DAY, 2);
        let web_new = r(web, NOW - DAY, 3);
        let web_failed_new = f(web, 4);
        let db_old = r(db, NOW - 2 * DAY, 5);
        let db_new = r(db, NOW - DAY, 6);
        // 손으로 만든 백업, 다른 작업의 백업, 만드는 중인 백업은 건드리지 않는다.
        let manual = backup("m", web, None, BackupPhase::Ready, Some(1));
        let other = backup("o", web, Some(Uuid::now_v7()), BackupPhase::Ready, Some(1));
        let pending = backup("p", web, Some(job), BackupPhase::Pending, None);
        let all = [
            web_failed_old.clone(),
            web_old.clone(),
            web_new.clone(),
            web_failed_new.clone(),
            db_old.clone(),
            db_new.clone(),
            manual,
            other,
            pending,
        ];
        let mut got = retention(job, &keep, &all);
        got.sort();
        let mut want = vec![web_old.id, web_failed_old.id, db_old.id];
        want.sort();
        assert_eq!(got, want);

        // 규칙이 모두 0이면 성공한 백업은 지우지 않지만, 더 새 성공 앞의 실패는 지운다.
        assert_eq!(retention(job, &Keep::default(), &all), [web_failed_old.id]);
        // 성공이 없으면 실패는 남는다.
        assert!(retention(job, &keep, &[web_failed_old, web_failed_new]).is_empty());
    }
}
```

`crates/opl-api/src/backups.rs`에서

```rust
/// [`check_backup`]을 거친 VM의 `pending` 백업 `name`을 만들고 노드 `node`에서 그것을 채울 Task를
/// 돌려준다. 백업 작업이 만드는 백업은 `job`에 작업 id를 적는다.
pub(crate) async fn start_backup(
```

을 다음으로 바꾼다:

```rust
/// [`check_backup`]을 거친 VM의 `pending` 백업 `name`을 만들고 노드 `node`에서 그것을 채울 Task를
/// 돌려준다. 이름이 올바르지 않으면 400. 백업 작업이 만드는 백업은 `job`에 작업 id를 적는다.
pub(crate) async fn start_backup(
```

`crates/opl-api/src/backups.rs`에서

```rust
) -> Result<Task, ApiError> {
    let backup: Backup = Resource::new(
```

을 다음으로 바꾼다:

```rust
) -> Result<Task, ApiError> {
    check_name(&name)?;
    let backup: Backup = Resource::new(
```

`crates/opl-api/src/lib.rs`에서

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use firmware::UefiVarsFetcher;
```

을 다음으로 바꾼다:

```rust
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use backup_jobs::{BackupJobScheduler, BackupJobSchedulerConfig};
pub use firmware::UefiVarsFetcher;
```

`crates/opl-api/tests/backup_job_runs.rs`를 만든다:

```rust
//! 백업 작업 스케줄러: 짧은 tick으로 실행을 시작하고 대상 VM을 고르며, 노드당 동시 백업 Task 수를
//! 지키고, 끝나면 `last_run`을 적고 보존 규칙대로 지우는지. 실패한 백업은 더 새 성공 뒤 지우고, 백업이
//! 진행 중인 VM과 거절된 VM, 없는 이름은 오류로 남기며, 놓친 실행은 한 번만 하고, 꺼 둔 작업은 돌지
//! 않는지. 백업을 채우지 못하고 끝난 Task, 실행 도중 없어진 VM, 상태에 적지 못한 이번 실행의 백업도.
//! Mock 하이퍼바이저와 dir 풀로 실제 백업 Task를 돌리므로 qemu-img가 필요하다.
//! `cargo test -p opl-api --features storage-it --test backup_job_runs`
#![cfg(feature = "storage-it")]

mod common;

use std::time::{Duration, Instant};

use common::{Client, Env, WAIT, vm_request};
use opl_api::{BackupJobScheduler, BackupJobSchedulerConfig};
use opl_types::backup::{Backup, BackupPhase, BackupSpec, BackupStatus};
use opl_types::backup_job::{BackupJob, BackupJobSpec, BackupJobStatus, JobError, JobRun};
use opl_types::resource::Resource;
use opl_types::storage::{PoolBackend, StoragePoolSpec};
use opl_types::task::{TaskOp, TaskSpec, TaskState, TaskStatus};
use opl_types::vm::VmSpec;
use reqwest::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

const MIB: u64 = 1 << 20;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// 50ms마다 보고 노드마다 백업 Task를 `max_concurrent`개까지 돌리는 스케줄러
fn scheduler(env: &Env, max_concurrent: usize) -> BackupJobScheduler {
    BackupJobScheduler::spawn(
        env.state.clone(),
        BackupJobSchedulerConfig {
            tick: Duration::from_millis(50),
            max_concurrent,
        },
    )
}

/// 이 노드에서 준비된 dir 풀을 저장소에 직접 만든다.
async fn create_pool(env: &Env, name: &str, backups: bool) {
    let path = env.dir.path().join(name);
    std::fs::create_dir_all(&path).unwrap();
    let pool = Resource::new(
        name,
        StoragePoolSpec {
            backend: PoolBackend::Dir { path },
            nodes: vec![],
            shared: false,
            backups,
        },
    );
    env.cluster.create(&pool).await.unwrap();
    env.pool_ready(name).await;
}

/// 풀 `local`과 백업 풀 `bk`, 그리고 `{이름}-root` qcow2 디스크를 붙인 꺼진 VM들. `labels`가 있는 VM은
/// 그 라벨을 단다.
async fn setup(env: &Env, admin: &Client, vms: &[(&str, Value)]) {
    create_pool(env, "local", false).await;
    create_pool(env, "bk", true).await;
    for (name, labels) in vms {
        let volume = format!("{name}-root");
        let (status, task) = admin
            .post(
                "/volumes",
                json!({
                    "name": volume,
                    "spec": { "pool": "local", "size_bytes": MIB, "format": "qcow2" }
                }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{task}");
        let task = admin.wait_task(&task).await;
        assert_eq!(task["status"]["state"], "succeeded", "{task}");
        let mut req = vm_request(name);
        req["labels"] = labels.clone();
        req["spec"]["disks"] = json!([{ "volume": volume, "bus": "virtio-blk" }]);
        let (status, vm) = admin.post("/vms", req).await;
        assert_eq!(status, StatusCode::CREATED, "{vm}");
    }
}

/// 지금부터 열두 시간 뒤의 `HH:MM`(UTC). 시험 도중 예약 시각이 오지 않게 한다.
fn far_at() -> String {
    let t = (now() + 12 * 3600) % 86400;
    format!("{:02}:{:02}", t / 3600, t / 60 % 60)
}

async fn create_job(admin: &Client, name: &str, vms: Value, keep: Value, enabled: bool) -> Uuid {
    let (status, body) = admin
        .post(
            "/backup-jobs",
            json!({
                "name": name,
                "spec": {
                    "vms": vms,
                    "pool": "bk",
                    "schedule": { "at": far_at() },
                    "keep": keep,
                    "enabled": enabled,
                }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

fn job(env: &Env, id: Uuid) -> BackupJob {
    env.cluster.get::<BackupJobSpec>(id).unwrap().unwrap()
}

/// 작업이 `done`을 만족할 때까지 기다린다.
async fn wait_job(env: &Env, id: Uuid, done: impl Fn(&BackupJob) -> bool) -> BackupJob {
    let deadline = Instant::now() + WAIT;
    loop {
        let job = job(env, id);
        if done(&job) {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "backup job did not settle: {job:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 실행이 없고 다음 실행이 앞으로 잡혔다(지금 실행 요청은 다음 실행을 지금으로 당긴다).
fn idle(job: &BackupJob) -> bool {
    job.status.run.is_none() && job.status.next_run.is_some_and(|t| t > now())
}

/// 지금 실행을 요청하고 그 실행이 끝날 때까지 기다린다.
async fn run_now(env: &Env, admin: &Client, id: Uuid) -> BackupJob {
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(env, id, idle).await
}

/// 작업 `id`가 만든 백업(만든 순서)
fn job_backups(env: &Env, id: Uuid) -> Vec<Backup> {
    let mut backups: Vec<Backup> = env
        .cluster
        .list::<BackupSpec>()
        .unwrap()
        .1
        .into_iter()
        .filter(|b| b.spec.job == Some(id))
        .collect();
    backups.sort_by_key(|b| b.id);
    backups
}

/// 백업들이 모두 끝날(ready나 failed) 때까지 기다린다.
async fn wait_backups_done(env: &Env, id: Uuid) -> Vec<Backup> {
    let deadline = Instant::now() + WAIT;
    loop {
        let backups = job_backups(env, id);
        if backups
            .iter()
            .all(|b| b.status.phase != BackupPhase::Pending)
        {
            return backups;
        }
        assert!(
            Instant::now() < deadline,
            "backups did not finish: {backups:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 백업 작업이 아닌 것을 포함한 끝나지 않은 VM 백업 Task 수
fn unfinished_backup_tasks(env: &Env) -> usize {
    env.cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmBackup { .. }) && !t.status.state.is_finished())
        .count()
}

/// 작업의 다음 실행 시각을 `at`으로 직접 바꾼다.
async fn set_next_run(env: &Env, id: Uuid, at: u64) {
    let job = job(env, id);
    let status = BackupJobStatus {
        next_run: Some(at),
        ..job.status
    };
    env.cluster
        .update_status_checked::<BackupJobSpec>(id, job.revision, &status)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_run_backs_up_the_selected_vms_and_records_the_result() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(
        &env,
        &admin,
        &[("web", json!({ "tier": "prod" })), ("db", json!({}))],
    )
    .await;
    let id = create_job(
        &admin,
        "nightly",
        json!({ "labels": { "tier": "prod" } }),
        json!({}),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    // 예약 시각 전에는 돌지 않는다.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job(&env, id).status.last_run, None);
    assert!(job_backups(&env, id).is_empty());

    let before = now();
    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.clone().unwrap();
    assert_eq!((last.ok, last.failed), (1, 0), "{done:?}");
    assert!(before <= last.started_at && last.started_at <= last.finished_at);
    assert_eq!(done.status.errors, []);
    // 라벨이 맞는 VM만 백업했고, 백업에 작업을 적고 기본 이름 규칙을 따른다.
    let backups = job_backups(&env, id);
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(backups[0].spec.vm_name, "web");
    assert_eq!(backups[0].status.phase, BackupPhase::Ready);
    assert!(backups[0].name.starts_with("web-"), "{}", backups[0].name);
    let all = env.cluster.list::<BackupSpec>().unwrap().1;
    assert!(all.iter().all(|b| b.spec.vm_name != "db"), "{all:?}");
    // 다음 실행은 실행을 시작한 시각 뒤의 예약 시각이다.
    assert_eq!(
        done.status.next_run,
        Some(opl_types::backup_job::next_run(
            &done.spec.schedule,
            last.started_at
        ))
    );
    sched.shutdown().await;
}

#[tokio::test]
async fn limits_backup_tasks_per_node() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // Task가 끝나지 않게 실행기를 멈춘다. 노드당 하나만 돌리므로 첫 VM(이름 순 db)만 꺼낸다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let running = wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let still = job(&env, id);
    assert_eq!(still.status.run, running.status.run);
    let run = still.status.run.unwrap();
    assert_eq!(run.queue.len(), 1);
    assert_eq!(unfinished_backup_tasks(&env), 1);
    assert_eq!(job_backups(&env, id)[0].spec.vm_name, "db");
    // 실행 중에는 지금 실행을 다시 요청할 수 없다.
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // 실행기가 돌면 나머지도 백업하고 실행이 끝난다.
    env.start_runner().await;
    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (2, 0));
    let names: Vec<String> = job_backups(&env, id)
        .into_iter()
        .map(|b| b.spec.vm_name)
        .collect();
    assert_eq!(names, ["db", "web"]);
    sched.shutdown().await;
}

#[tokio::test]
async fn retention_deletes_backups_beyond_keep() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 손으로 만든 백업은 보존 규칙이 건드리지 않는다.
    let (status, task) = admin
        .post(
            "/vms/web/backups",
            json!({ "pool": "bk", "name": "manual" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let id = create_job(
        &admin,
        "keep2",
        json!({ "names": ["web"] }),
        json!({ "last": 2 }),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    let mut made = Vec::new();
    for _ in 0..3 {
        run_now(&env, &admin, id).await;
        let new: Vec<Uuid> = job_backups(&env, id)
            .into_iter()
            .map(|b| b.id)
            .filter(|b| !made.contains(b))
            .collect();
        made.extend(new);
    }
    assert_eq!(made.len(), 3);
    // 세 번째 실행이 끝나면 가장 오래된 것을 지운다.
    let deadline = Instant::now() + WAIT;
    while job_backups(&env, id).len() > 2 {
        assert!(Instant::now() < deadline, "{:?}", job_backups(&env, id));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let left: Vec<Uuid> = job_backups(&env, id).into_iter().map(|b| b.id).collect();
    assert_eq!(left, made[1..]);
    // 이름은 겹치지 않는다(같은 초에 만들면 `-2`…를 붙인다).
    let backups = job_backups(&env, id);
    assert_ne!(backups[0].name, backups[1].name);
    assert!(
        env.cluster
            .get_by_name::<BackupSpec>("manual")
            .unwrap()
            .is_some()
    );
    sched.shutdown().await;
}

#[tokio::test]
async fn failed_backups_count_and_go_after_a_newer_success() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 실행 중인 VM의 백업은 임시 스냅샷을 읽는데, Mock 하이퍼바이저는 파일에 스냅샷을 만들지 않으므로
    // 실패한다.
    let task = admin.act("web", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 1));
    assert_eq!(done.status.errors.len(), 1, "{:?}", done.status.errors);
    assert_eq!(done.status.errors[0].vm, "web");
    assert!(
        done.status.errors[0]
            .error
            .contains("cannot back up volume"),
        "{:?}",
        done.status.errors
    );
    let failed = job_backups(&env, id);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].status.phase, BackupPhase::Failed);

    // 다시 실패해도 더 새 성공이 없으므로 남는다.
    run_now(&env, &admin, id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(job_backups(&env, id).len(), 2);

    // 꺼진 VM은 백업된다. 성공하면 앞의 실패한 백업들을 지운다(보존 규칙이 없어도).
    let task = admin.act("web", "stop").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let deadline = Instant::now() + WAIT;
    while job_backups(&env, id).len() > 1 {
        assert!(Instant::now() < deadline, "{:?}", job_backups(&env, id));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(job_backups(&env, id)[0].status.phase, BackupPhase::Ready);
    sched.shutdown().await;
}

#[tokio::test]
async fn skips_vms_being_backed_up_and_records_refusals() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    // 실행 중이고 raw 디스크(dir 풀에서 스냅샷 불가)인 VM은 백업 요청이 거절된다.
    let (status, task) = admin
        .post(
            "/volumes",
            json!({
                "name": "db-root",
                "spec": { "pool": "local", "size_bytes": MIB, "format": "raw" }
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    admin.wait_task(&task).await;
    let mut req = vm_request("db");
    req["spec"]["disks"] = json!([{ "volume": "db-root", "bus": "virtio-blk" }]);
    let (status, vm) = admin.post("/vms", req).await;
    assert_eq!(status, StatusCode::CREATED, "{vm}");
    let task = admin.act("db", "start").await;
    assert_eq!(task["status"]["state"], "succeeded", "{task}");
    // web은 손으로 요청한 백업이 끝나지 않았다.
    env.stop_runner().await;
    let (status, task) = admin
        .post(
            "/vms/web/backups",
            json!({ "pool": "bk", "name": "manual" }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{task}");
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 2));
    assert_eq!(done.status.errors.len(), 2, "{:?}", done.status.errors);
    assert_eq!(done.status.errors[0].vm, "db");
    assert!(
        done.status.errors[0]
            .error
            .contains("cannot be snapshotted"),
        "{:?}",
        done.status.errors
    );
    assert_eq!(
        done.status.errors[1],
        JobError {
            vm: "web".into(),
            error: "a backup of this VM is already running".into(),
        }
    );
    assert!(job_backups(&env, id).is_empty());
    assert_eq!(unfinished_backup_tasks(&env), 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn missed_runs_run_once() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    // 사흘 동안 리더가 없었던 것처럼 다음 실행이 한참 지났다.
    set_next_run(&env, id, now() - 3 * 86400).await;
    let sched = scheduler(&env, 2);

    let done = wait_job(&env, id, |j| j.status.last_run.is_some() && idle(j)).await;
    wait_backups_done(&env, id).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(job(&env, id).status.last_run, done.status.last_run);
    assert_eq!(job_backups(&env, id).len(), 1);
    assert_eq!(done.status.last_run.unwrap().ok, 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn disabled_jobs_do_not_start_and_stop_taking_vms() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let off = create_job(&admin, "off", json!({ "all": true }), json!({}), false).await;
    set_next_run(&env, off, now() - 60).await;
    let on = create_job(&admin, "on", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // 꺼 둔 작업은 다음 실행이 지나도 돌지 않는다.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let job_off = job(&env, off);
    assert_eq!(job_off.status.run, None);
    assert_eq!(job_off.status.last_run, None);
    assert!(job_backups(&env, off).is_empty());

    // 실행 도중 끄면 이미 시작한 백업만 기다리고 남은 VM은 꺼내지 않는다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{on}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, on, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let (status, body) = admin
        .patch("/backup-jobs/on", json!({ "enabled": false }), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, on, |j| {
        j.status.run.as_ref().is_some_and(|r| r.queue.is_empty())
    })
    .await;
    env.start_runner().await;
    let done = wait_job(&env, on, |j| j.status.run.is_none()).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(job_backups(&env, on).len(), 1);
    sched.shutdown().await;
}

#[tokio::test]
async fn a_backup_whose_task_ended_unfinished_fails_the_run() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 2);

    // 실행기가 멈춘 사이 janitor가 죽은 노드의 Task를 실패로 처리한 것처럼 Task만 끝낸다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let running = wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let backup = running.status.run.unwrap().backups[0];
    let task = env
        .cluster
        .list::<TaskSpec>()
        .unwrap()
        .1
        .into_iter()
        .find(|t| matches!(t.spec.op, TaskOp::VmBackup { backup: b } if b == backup))
        .unwrap();
    let failed = TaskStatus {
        state: TaskState::Failed,
        error: Some("node lost".into()),
    };
    env.cluster
        .update_status::<TaskSpec>(task.id, &failed)
        .await
        .unwrap();

    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (0, 1));
    assert_eq!(
        done.status.errors,
        [JobError {
            vm: "web".into(),
            error: "backup task ended without finishing the backup".into(),
        }]
    );
    assert_eq!(job_backups(&env, id)[0].status.phase, BackupPhase::Pending);
    sched.shutdown().await;
}

#[tokio::test]
async fn takes_in_a_backup_of_this_run_left_out_of_the_status() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    // 실행이 web의 백업을 만들었지만(이전 리더) 상태에 적기 전에 멈췄다.
    let job = job(&env, id);
    let status = BackupJobStatus {
        run: Some(JobRun {
            started_at: now() - 5,
            queue: vec![web.id],
            backups: vec![],
        }),
        ..job.status
    };
    env.cluster
        .update_status_checked::<BackupJobSpec>(id, job.revision, &status)
        .await
        .unwrap();
    let mut made: Backup = Resource::new(
        "web-made",
        BackupSpec {
            vm: web.id,
            vm_name: web.name.clone(),
            pool: "bk".into(),
            vm_spec: web.spec.clone(),
            job: Some(id),
        },
    );
    made.status = BackupStatus {
        phase: BackupPhase::Ready,
        created_at: Some(now()),
        ..Default::default()
    };
    env.cluster.create(&made).await.unwrap();
    let sched = scheduler(&env, 2);

    // 새 백업을 만들지 않고 그 백업을 이번 실행의 것으로 받아들인다.
    let done = wait_job(&env, id, |j| j.status.run.is_none()).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let ids: Vec<Uuid> = job_backups(&env, id).into_iter().map(|b| b.id).collect();
    assert_eq!(ids, [made.id]);
    assert_eq!(unfinished_backup_tasks(&env), 0);
    sched.shutdown().await;
}

#[tokio::test]
async fn names_without_a_vm_are_errors() {
    let env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({}))]).await;
    let id = create_job(
        &admin,
        "named",
        json!({ "names": ["web", "ghost"] }),
        json!({}),
        true,
    )
    .await;
    let sched = scheduler(&env, 2);

    let done = run_now(&env, &admin, id).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 1));
    assert_eq!(
        done.status.errors,
        [JobError {
            vm: "ghost".into(),
            error: "no VM named \"ghost\"".into(),
        }]
    );
    sched.shutdown().await;
}

#[tokio::test]
async fn vms_deleted_during_a_run_are_skipped() {
    let mut env = Env::start().await;
    let admin = env.admin().await;
    setup(&env, &admin, &[("web", json!({})), ("db", json!({}))]).await;
    let id = create_job(&admin, "all", json!({ "all": true }), json!({}), true).await;
    let sched = scheduler(&env, 1);

    // 노드당 하나만 돌리므로 db를 백업하는 동안 web은 큐에 남는다. 그사이 web이 없어진다.
    env.stop_runner().await;
    let (status, body) = admin
        .post(&format!("/backup-jobs/{id}/run"), Value::Null)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_job(&env, id, |j| {
        j.status.run.as_ref().is_some_and(|r| r.backups.len() == 1)
    })
    .await;
    let web = env.cluster.get_by_name::<VmSpec>("web").unwrap().unwrap();
    env.cluster.delete::<VmSpec>(web.id, None).await.unwrap();
    env.start_runner().await;

    let done = wait_job(&env, id, idle).await;
    let last = done.status.last_run.unwrap();
    assert_eq!((last.ok, last.failed), (1, 0));
    assert_eq!(done.status.errors, []);
    let names: Vec<String> = job_backups(&env, id)
        .into_iter()
        .map(|b| b.spec.vm_name)
        .collect();
    assert_eq!(names, ["db"]);
    sched.shutdown().await;
}
```

`crates/opld/src/config.rs` 전체:

```rust
use std::net::{IpAddr, SocketAddr};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use opl_api::{BackupJobSchedulerConfig, ClusterJanitorConfig, SnapshotSchedulerConfig};
use opl_qemu::{Accel, HostProfile, kvm_available};
use opl_reconciler::HaConfig;
use opl_reconciler::ha::{DeviceWatchdog, WatchdogOpener};
use opl_types::node::{Fencing, NodeHa};
use serde::Deserialize;

/// `opld --config <path>`로 읽는 TOML 설정.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// 노드 이름. 클러스터 안에서 유일해야 한다.
    pub name: String,
    /// 다른 노드와 클라이언트가 이 노드에 닿는 호스트(IP나 DNS 이름). 노드 인증서에 들어가고,
    /// 클러스터에 알리는 주소(`address:포트`)가 된다. 나중에 바꿀 수 없다.
    pub address: String,
    /// 위트니스 노드: VM을 돌리지 않고 선거에 나서지 않는 Raft 투표 멤버. 합류만 할 수 있고, HTTPS
    /// API를 열지 않으며 `[hypervisor]`·`[pools]`·`[migration]`을 쓰지 않는다(QEMU가 없어도 된다).
    #[serde(default)]
    pub witness: bool,
    /// API(HTTPS)를 받을 소켓 주소
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    /// 노드 간 통신(gRPC over mTLS)을 받을 소켓 주소
    #[serde(default = "default_cluster_listen")]
    pub cluster_listen: SocketAddr,
    /// `oplctl cluster init|join|leave`를 받는 유닉스 소켓. 기본은 `{run_dir}/opld.sock`이다.
    /// opld를 실행한 사용자만 쓸 수 있다(0600).
    pub control_socket: Option<PathBuf>,
    /// 클러스터 저장소, 노드 인증서, VM별 영구 파일, 올린 이미지를 둔다.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// QEMU 소켓과 pidfile을 두는 휘발성 디렉터리. 유닉스 소켓 경로 길이 제한(104바이트) 때문에 짧아야 한다.
    #[serde(default = "default_run_dir")]
    pub run_dir: PathBuf,
    /// Homebrew QEMU의 펌웨어 디렉터리(macOS). Linux는 Debian 경로를 쓴다(`[hypervisor]`로 바꾼다).
    #[serde(default = "default_qemu_share_dir")]
    pub qemu_share_dir: PathBuf,
    #[serde(default = "default_qemu_img")]
    pub qemu_img: PathBuf,
    #[serde(default)]
    pub hypervisor: HypervisorSection,
    #[serde(default)]
    pub reconciler: ReconcilerSection,
    #[serde(default)]
    pub schedule: ScheduleSection,
    #[serde(default)]
    pub janitor: JanitorSection,
    #[serde(default)]
    pub backup: BackupSection,
    #[serde(default)]
    pub pools: PoolsSection,
    #[serde(default)]
    pub migration: MigrationSection,
    #[serde(default)]
    pub ha: HaSection,
    #[serde(default)]
    pub network: NetworkSection,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HypervisorSection {
    /// `auto`(macOS는 hvf, Linux는 KVM이 있으면 kvm, 없으면 tcg), `kvm`, `tcg`, `hvf`
    #[serde(default)]
    pub accel: AccelSetting,
    /// QEMU 실행 파일. 기본은 호스트에 맞는 `qemu-system-*`이다.
    pub qemu_binary: Option<PathBuf>,
    /// UEFI 펌웨어 코드와 VM마다 복사하는 변수 저장소 원본
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
    /// QEMU를 `systemd-run --scope`로 감싼다(VM마다 cgroup). Linux에서 기본으로 켠다.
    #[serde(default = "default_systemd_scope")]
    pub systemd_scope: bool,
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            accel: AccelSetting::default(),
            qemu_binary: None,
            uefi_code: None,
            uefi_vars: None,
            systemd_scope: default_systemd_scope(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccelSetting {
    #[default]
    Auto,
    Kvm,
    Tcg,
    Hvf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilerSection {
    /// ACPI 종료를 요청한 뒤 강제 종료하기까지 기다리는 시간(초)
    #[serde(default = "default_acpi_timeout")]
    pub acpi_timeout_secs: u64,
}

impl Default for ReconcilerSection {
    fn default() -> Self {
        Self {
            acpi_timeout_secs: default_acpi_timeout(),
        }
    }
}

impl ReconcilerSection {
    pub fn acpi_timeout(&self) -> Duration {
        Duration::from_secs(self.acpi_timeout_secs)
    }
}

/// 예약 스냅샷 작업. 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSection {
    /// 예약을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 주기의 한 시간(초). 매일은 24배, 매주는 168배다.
    #[serde(default = "default_hour_secs")]
    pub hour_secs: u64,
}

impl Default for ScheduleSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            hour_secs: default_hour_secs(),
        }
    }
}

impl ScheduleSection {
    pub fn scheduler_config(&self) -> SnapshotSchedulerConfig {
        SnapshotSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            hour: Duration::from_secs(self.hour_secs),
        }
    }
}

/// 클러스터 정리 작업. 간격·유예는 테스트만 줄인다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JanitorSection {
    /// 목록을 확인하는 간격(초)
    #[serde(default = "default_janitor_tick_secs")]
    pub tick_secs: u64,
    /// 오프라인 노드의 `pending` Task를 실패로 끝내기까지 기다리는 시간(초)
    #[serde(default = "default_offline_grace_secs")]
    pub offline_grace_secs: u64,
    /// 만든 지 이 날수가 넘은 끝난 Task를 지운다.
    #[serde(default = "default_task_max_age_days")]
    pub task_max_age_days: u64,
    /// 끝난 Task를 이보다 많이 남기지 않는다(넘으면 오래된 것부터 지운다).
    #[serde(default = "default_task_max_finished")]
    pub task_max_finished: usize,
}

impl Default for JanitorSection {
    fn default() -> Self {
        Self {
            tick_secs: default_janitor_tick_secs(),
            offline_grace_secs: default_offline_grace_secs(),
            task_max_age_days: default_task_max_age_days(),
            task_max_finished: default_task_max_finished(),
        }
    }
}

impl JanitorSection {
    pub fn janitor_config(&self) -> ClusterJanitorConfig {
        ClusterJanitorConfig {
            tick: Duration::from_secs(self.tick_secs),
            offline_grace: Duration::from_secs(self.offline_grace_secs),
            task_max_age: Duration::from_secs(self.task_max_age_days * 24 * 3600),
            task_max_finished: self.task_max_finished,
            // 고아 스냅샷·끝난 Task의 유예는 설정하지 않는다(기본 30초·10분).
            ..ClusterJanitorConfig::default()
        }
    }
}

/// 백업 작업 스케줄러(리더)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupSection {
    /// 작업을 확인하는 간격(초)
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// 노드마다 동시에 도는 백업 Task의 최대 수
    #[serde(default = "default_backup_max_concurrent")]
    pub max_concurrent: usize,
}

impl Default for BackupSection {
    fn default() -> Self {
        Self {
            tick_secs: default_tick_secs(),
            max_concurrent: default_backup_max_concurrent(),
        }
    }
}

impl BackupSection {
    pub fn scheduler_config(&self) -> BackupJobSchedulerConfig {
        BackupJobSchedulerConfig {
            tick: Duration::from_secs(self.tick_secs),
            max_concurrent: self.max_concurrent,
        }
    }
}

/// 노드별 풀 상태 새로 고침(NFS 마운트 포함)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolsSection {
    /// 풀 상태를 확인하는 간격(초)
    #[serde(default = "default_pools_refresh_secs")]
    pub refresh_secs: u64,
}

impl Default for PoolsSection {
    fn default() -> Self {
        Self {
            refresh_secs: default_pools_refresh_secs(),
        }
    }
}

impl PoolsSection {
    pub fn refresh(&self) -> Duration {
        Duration::from_secs(self.refresh_secs)
    }
}

/// 라이브 마이그레이션(설계 §4)
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSection {
    /// 받는 쪽 QEMU가 마이그레이션을 받는 포트 범위(`"A-B"`, 둘 다 포함). 노드 사이 방화벽에서
    /// 열어야 한다.
    #[serde(
        default = "default_migration_ports",
        deserialize_with = "deserialize_port_range"
    )]
    pub ports: RangeInclusive<u16>,
    /// 보내는 대역폭 상한(MiB/s). 0이면 QEMU 기본값이다.
    #[serde(default)]
    pub bandwidth_mib: u64,
    /// 마이그레이션이 이 시간 안에 끝나지 않으면 취소한다(`"30m"`, `"1h30m"`, `"90s"`).
    #[serde(
        default = "default_migration_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub timeout: Duration,
}

impl Default for MigrationSection {
    fn default() -> Self {
        Self {
            ports: default_migration_ports(),
            bandwidth_mib: 0,
            timeout: default_migration_timeout(),
        }
    }
}

/// HA 리스와 자기 펜싱(설계 §3.1). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaSection {
    /// `watchdog`(opld가 HA VM을 끄고, opld가 멈추면 watchdog가 노드를 재시작), `process`(opld가
    /// 끄는 것만 — 시험·개발용), `off`(이 노드는 HA VM을 돌리지 않는다). 기본은 Linux `watchdog`,
    /// macOS `off`다.
    #[serde(default)]
    pub fencing: FencingSetting,
    /// Linux watchdog 장치(하드웨어 또는 `softdog`)
    #[serde(default = "default_watchdog_device")]
    pub watchdog_device: PathBuf,
    /// 장치에 정하는 제한 시간(초, `WDIOC_SETTIMEOUT`)
    #[serde(default = "default_watchdog_timeout_secs")]
    pub watchdog_timeout_secs: u64,
    /// 마지막으로 성공한 리스 갱신 뒤 이만큼 지나면 HA VM을 끈다(초).
    #[serde(default = "default_fence_after_secs")]
    pub fence_after_secs: u64,
}

impl Default for HaSection {
    fn default() -> Self {
        Self {
            fencing: FencingSetting::default(),
            watchdog_device: default_watchdog_device(),
            watchdog_timeout_secs: default_watchdog_timeout_secs(),
            fence_after_secs: default_fence_after_secs(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FencingSetting {
    Watchdog,
    Process,
    Off,
}

impl Default for FencingSetting {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Self::Watchdog
        } else {
            Self::Off
        }
    }
}

/// `watchdog_timeout_secs`의 하한. 1초마다 쓰다듬으므로 한두 번 늦어도 재시작되지 않게 둔다.
const MIN_WATCHDOG_TIMEOUT_SECS: u64 = 5;
/// `watchdog_timeout_secs`의 상한(1시간)
const MAX_WATCHDOG_TIMEOUT_SECS: u64 = 3600;
/// 리더가 옮기기 전에 더 기다리는 여유(초)
const RECOVER_MARGIN_SECS: u64 = 5;

impl HaSection {
    /// 등록할 때 쓰는 이 노드의 HA 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn node_ha(&self) -> Option<NodeHa> {
        let (fencing, watchdog) = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => (Fencing::Watchdog, self.watchdog_timeout_secs),
            FencingSetting::Process => (Fencing::Process, 0),
        };
        Some(NodeHa {
            fencing,
            recover_after_secs: self
                .fence_after_secs
                .saturating_add(watchdog)
                .saturating_add(RECOVER_MARGIN_SECS),
        })
    }
}

impl HaSection {
    pub fn fence_after(&self) -> Duration {
        Duration::from_secs(self.fence_after_secs)
    }

    /// 리스 갱신 간격: `fence_after`의 10분의 1을 0.2초 이상 2초 이하로 자른 값
    pub fn renew_interval(&self) -> Duration {
        (self.fence_after() / 10).clamp(Duration::from_millis(200), Duration::from_secs(2))
    }

    /// 노드의 `HaGuard` 설정. 펜싱이 꺼져 있으면 None이다.
    pub fn guard_config(&self) -> Option<HaConfig> {
        let watchdog: Option<Arc<dyn WatchdogOpener>> = match self.fencing {
            FencingSetting::Off => return None,
            FencingSetting::Watchdog => Some(Arc::new(DeviceWatchdog {
                path: self.watchdog_device.clone(),
                timeout_secs: self.watchdog_timeout_secs,
            })),
            FencingSetting::Process => None,
        };
        Some(HaConfig {
            fence_after: self.fence_after(),
            renew_interval: self.renew_interval(),
            watchdog,
        })
    }
}

/// 노드 네트워크(VXLAN). 위트니스는 쓰지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// VXLAN 패킷을 주고받을 이 노드의 주소. 없으면 `address`가 IP면 그것, 이름이면 시작할 때 푼
    /// 주소(loopback·link-local은 빼고 IPv4 먼저)다.
    #[serde(default)]
    pub underlay: Option<IpAddr>,
}

/// 노드 underlay를 정하는 방법
#[derive(Debug, PartialEq, Eq)]
pub enum UnderlaySource<'a> {
    /// 설정했거나 `address`가 IP 리터럴이다.
    Ip(IpAddr),
    /// `address`가 이름이라 풀어야 한다.
    Lookup(&'a str),
}

fn default_watchdog_device() -> PathBuf {
    "/dev/watchdog".into()
}
fn default_watchdog_timeout_secs() -> u64 {
    30
}
fn default_fence_after_secs() -> u64 {
    30
}

fn default_migration_ports() -> RangeInclusive<u16> {
    60000..=60099
}
fn default_migration_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}

/// `"A-B"`: 0이 아닌 포트 A부터 B까지(A ≤ B)
fn parse_port_range(s: &str) -> Result<RangeInclusive<u16>, String> {
    let err =
        || format!("invalid port range {s:?} (expected \"FIRST-LAST\", e.g. \"60000-60099\")");
    let (a, b) = s.split_once('-').ok_or_else(err)?;
    let a: u16 = a.trim().parse().map_err(|_| err())?;
    let b: u16 = b.trim().parse().map_err(|_| err())?;
    if a == 0 || a > b {
        return Err(err());
    }
    Ok(a..=b)
}

/// 숫자와 단위(`h`, `m`, `s`)를 이어 쓴 시간: `"30m"`, `"1h30m"`. 0보다 커야 한다.
fn parse_duration(s: &str) -> Result<Duration, String> {
    let err = || format!("invalid duration {s:?} (e.g. \"30m\", \"1h30m\", \"90s\")");
    let mut rest = s.trim();
    let mut secs: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
        let n: u64 = rest[..digits].parse().map_err(|_| err())?;
        let unit = match rest.as_bytes()[digits] {
            b'h' => 3600,
            b'm' => 60,
            b's' => 1,
            _ => return Err(err()),
        };
        secs = n
            .checked_mul(unit)
            .and_then(|v| secs.checked_add(v))
            .ok_or_else(err)?;
        rest = &rest[digits + 1..];
    }
    if secs == 0 {
        return Err(err());
    }
    Ok(Duration::from_secs(secs))
}

fn deserialize_port_range<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<RangeInclusive<u16>, D::Error> {
    parse_port_range(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn deserialize_duration<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    parse_duration(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn default_listen() -> SocketAddr {
    "0.0.0.0:8006".parse().unwrap()
}
fn default_cluster_listen() -> SocketAddr {
    "0.0.0.0:8007".parse().unwrap()
}
fn default_data_dir() -> PathBuf {
    "/var/lib/opl".into()
}
fn default_run_dir() -> PathBuf {
    "/run/opl".into()
}
fn default_qemu_share_dir() -> PathBuf {
    "/opt/homebrew/share/qemu".into()
}
fn default_systemd_scope() -> bool {
    cfg!(target_os = "linux")
}
fn default_qemu_img() -> PathBuf {
    "qemu-img".into()
}
fn default_acpi_timeout() -> u64 {
    60
}
fn default_tick_secs() -> u64 {
    60
}
fn default_hour_secs() -> u64 {
    3600
}
fn default_backup_max_concurrent() -> usize {
    2
}
fn default_janitor_tick_secs() -> u64 {
    10
}
fn default_offline_grace_secs() -> u64 {
    60
}
/// `task_max_age_days`의 상한(100년)
const MAX_TASK_AGE_DAYS: u64 = 36500;
fn default_task_max_age_days() -> u64 {
    7
}
fn default_task_max_finished() -> usize {
    1000
}
fn default_pools_refresh_secs() -> u64 {
    15
}

/// `host:port`. IPv6 주소는 대괄호로 감싼다.
fn with_port(host: &str, port: u16) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display()))?;
        let config: Self = toml::from_str(&text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> anyhow::Result<()> {
        opl_types::resource::validate_name(&self.name)?;
        let address = &self.address;
        let ok = !address.is_empty()
            && (address.parse::<IpAddr>().is_ok()
                || address
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.'));
        if !ok {
            anyhow::bail!(
                "address must be an IP address or a host name without a port (got {address:?})"
            );
        }
        // 다른 노드가 닿을 주소이므로 0.0.0.0이나 ::은 안 된다.
        if address
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_unspecified())
        {
            anyhow::bail!("address must not be an unspecified address (got {address:?})");
        }
        for (key, addr) in [
            ("listen", self.listen),
            ("cluster_listen", self.cluster_listen),
        ] {
            if addr.port() == 0 {
                anyhow::bail!("{key} must have a port other than 0 (got {addr})");
            }
        }
        let linux = cfg!(target_os = "linux");
        match self.hypervisor.accel {
            AccelSetting::Kvm if !linux => anyhow::bail!("accel = \"kvm\" needs Linux"),
            AccelSetting::Hvf if linux => anyhow::bail!("accel = \"hvf\" needs macOS"),
            _ => {}
        }
        if self.hypervisor.systemd_scope && !linux {
            anyhow::bail!("systemd_scope needs Linux; set it to false");
        }
        if self.schedule.tick_secs == 0 || self.schedule.hour_secs == 0 {
            anyhow::bail!("schedule.tick_secs and schedule.hour_secs must be at least 1");
        }
        if self.janitor.tick_secs == 0 || self.janitor.offline_grace_secs == 0 {
            anyhow::bail!("janitor.tick_secs and janitor.offline_grace_secs must be at least 1");
        }
        // 0이면 끝난 Task를 곧바로 지워 Task를 기다리는 쪽이 결과를 못 본다.
        if self.janitor.task_max_age_days == 0 || self.janitor.task_max_finished == 0 {
            anyhow::bail!(
                "janitor.task_max_age_days and janitor.task_max_finished must be at least 1"
            );
        }
        // 날수를 초로 바꿀 때 넘치지 않도록 100년까지만 받는다.
        if self.janitor.task_max_age_days > MAX_TASK_AGE_DAYS {
            anyhow::bail!("janitor.task_max_age_days must be at most {MAX_TASK_AGE_DAYS}");
        }
        if self.backup.tick_secs == 0 || self.backup.max_concurrent == 0 {
            anyhow::bail!("backup.tick_secs and backup.max_concurrent must be at least 1");
        }
        if self.pools.refresh_secs == 0 {
            anyhow::bail!("pools.refresh_secs must be at least 1");
        }
        if self.ha.fence_after_secs == 0 {
            anyhow::bail!("ha.fence_after_secs must be at least 1");
        }
        if self.ha.fencing == FencingSetting::Watchdog {
            if !(MIN_WATCHDOG_TIMEOUT_SECS..=MAX_WATCHDOG_TIMEOUT_SECS)
                .contains(&self.ha.watchdog_timeout_secs)
            {
                anyhow::bail!(
                    "ha.watchdog_timeout_secs must be between {MIN_WATCHDOG_TIMEOUT_SECS} and {MAX_WATCHDOG_TIMEOUT_SECS}"
                );
            }
            if !linux {
                anyhow::bail!("ha.fencing = \"watchdog\" needs Linux; use \"process\" or \"off\"");
            }
        }
        // 다른 노드가 VXLAN 패킷을 보낼 주소이므로 0.0.0.0이나 ::은 안 된다.
        if let Some(ip) = self.network.underlay
            && ip.is_unspecified()
        {
            anyhow::bail!("network.underlay must not be an unspecified address (got {ip})");
        }
        // 알리는 주소는 `address`에 포트만 붙이므로 포트가 같으면 둘을 구별할 수 없다.
        if self.listen.port() == self.cluster_listen.port() {
            anyhow::bail!(
                "listen and cluster_listen must use different ports (got {} for both)",
                self.listen.port()
            );
        }
        Ok(())
    }

    /// 이 호스트에서 QEMU를 실행하는 방법. `accel = "kvm"`인데 `/dev/kvm`을 열 수 없으면 오류다.
    pub fn host_profile(&self) -> anyhow::Result<HostProfile> {
        let h = &self.hypervisor;
        let mut profile = HostProfile::native(&self.qemu_share_dir);
        match h.accel {
            AccelSetting::Auto => {}
            AccelSetting::Kvm if !kvm_available() => {
                anyhow::bail!("accel = \"kvm\" but /dev/kvm cannot be opened")
            }
            AccelSetting::Kvm => profile.accel = Accel::Kvm,
            AccelSetting::Tcg => profile.accel = Accel::Tcg,
            AccelSetting::Hvf => profile.accel = Accel::Hvf,
        }
        if let Some(binary) = &h.qemu_binary {
            profile.qemu_binary = binary.clone();
        }
        if let Some(code) = &h.uefi_code {
            profile.uefi_code = code.clone();
        }
        if let Some(vars) = &h.uefi_vars {
            profile.uefi_vars_template = vars.clone();
        }
        Ok(profile)
    }

    /// 클러스터에 알리는 API 주소
    pub fn api_address(&self) -> String {
        with_port(&self.address, self.listen.port())
    }

    /// 클러스터에 알리는 노드 간 통신 주소(Raft 멤버십의 노드 주소)
    pub fn cluster_address(&self) -> String {
        with_port(&self.address, self.cluster_listen.port())
    }

    /// 마이그레이션을 받는 QEMU가 들을 호스트: 노드 간 통신을 받는 IP(`cluster_listen`, 기본
    /// `0.0.0.0`). `address`는 원본이 접속하는 이름일 뿐 이 기계의 주소로 풀리지 않을 수 있다(예:
    /// `/etc/hosts`의 `127.0.1.1`).
    pub fn migration_listen_host(&self) -> String {
        self.cluster_listen.ip().to_string()
    }

    /// 노드 인증서의 이름: 알리는 호스트와 같은 기계에서 쓰는 이름
    pub fn cert_names(&self) -> Vec<String> {
        let mut names = vec![self.address.clone()];
        for local in ["localhost", "127.0.0.1", "::1"] {
            if self.address != local {
                names.push(local.into());
            }
        }
        names
    }

    /// underlay를 정하는 방법: `[network] underlay`, 없으면 `address`
    pub fn underlay_source(&self) -> UnderlaySource<'_> {
        if let Some(ip) = self.network.underlay {
            return UnderlaySource::Ip(ip);
        }
        match self.address.parse::<IpAddr>() {
            Ok(ip) => UnderlaySource::Ip(ip),
            Err(_) => UnderlaySource::Lookup(&self.address),
        }
    }

    /// Node status에 기록할 underlay. 이름을 풀지 못하면 경고하고 None이다(이 노드에 VXLAN 장치를
    /// 만들지 않는다).
    pub async fn underlay(&self) -> Option<IpAddr> {
        self.underlay_with(|host| async move {
            let addrs = tokio::net::lookup_host((host.as_str(), 0)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
        .await
    }

    /// `underlay`와 같되 이름 풀기(`lookup`)를 받는다(시험이 DNS에 기대지 않게).
    async fn underlay_with<F, Fut>(&self, lookup: F) -> Option<IpAddr>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = std::io::Result<Vec<IpAddr>>>,
    {
        let host = match self.underlay_source() {
            UnderlaySource::Ip(ip) => return Some(ip),
            UnderlaySource::Lookup(host) => host,
        };
        match lookup(host.to_string()).await {
            Ok(addrs) => match pick_underlay(addrs) {
                Some(ip) => Some(ip),
                None => {
                    tracing::warn!(
                        host,
                        "address has no usable IP (only loopback or link-local); set [network] underlay for VXLAN"
                    );
                    None
                }
            },
            Err(e) => {
                tracing::warn!(
                    host,
                    error = %e,
                    "cannot resolve address; set [network] underlay for VXLAN"
                );
                None
            }
        }
    }

    pub fn control_socket(&self) -> PathBuf {
        self.control_socket
            .clone()
            .unwrap_or_else(|| self.run_dir.join("opld.sock"))
    }
}

/// 이름을 푼 주소 가운데 underlay로 쓸 것. 다른 노드가 닿을 수 없는 loopback·link-local·미지정 주소는
/// 빼고(예: `/etc/hosts`의 `127.0.1.1`), IPv4를 먼저, 없으면 IPv6의 첫 주소다.
fn pick_underlay(addrs: impl IntoIterator<Item = IpAddr>) -> Option<IpAddr> {
    let usable: Vec<IpAddr> = addrs
        .into_iter()
        .filter(|ip| {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && match ip {
                    IpAddr::V4(v4) => !v4.is_link_local(),
                    IpAddr::V6(v6) => !v6.is_unicast_link_local(),
                }
        })
        .collect();
    let v4 = usable.iter().find(|ip| ip.is_ipv4());
    v4.or(usable.first()).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn fills_defaults() {
        let c = parse("name = \"node1\"\naddress = \"10.0.0.5\"");
        assert!(!c.witness);
        assert_eq!(c.listen, "0.0.0.0:8006".parse().unwrap());
        assert_eq!(c.api_address(), "10.0.0.5:8006");
        assert_eq!(c.cluster_address(), "10.0.0.5:8007");
        assert_eq!(c.data_dir, Path::new("/var/lib/opl"));
        assert_eq!(c.control_socket(), Path::new("/run/opl/opld.sock"));
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(60));
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(60));
        assert_eq!(schedule.hour, Duration::from_secs(3600));
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(10));
        assert_eq!(janitor.offline_grace, Duration::from_secs(60));
        assert_eq!(janitor.orphan_grace, Duration::from_secs(30));
        assert_eq!(janitor.task_max_age, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 1000);
        assert_eq!(janitor.finished_grace, Duration::from_secs(600));
        let backup = c.backup.scheduler_config();
        assert_eq!(backup.tick, Duration::from_secs(60));
        assert_eq!(backup.max_concurrent, 2);
        assert_eq!(c.pools.refresh(), Duration::from_secs(15));
        assert_eq!(c.migration.ports, 60000..=60099);
        assert_eq!(c.migration.bandwidth_mib, 0);
        assert_eq!(c.migration.timeout, Duration::from_secs(30 * 60));
        assert_eq!(c.hypervisor.accel, AccelSetting::Auto);
        assert_eq!(c.hypervisor.systemd_scope, cfg!(target_os = "linux"));
        assert_eq!(
            c.cert_names(),
            ["10.0.0.5", "localhost", "127.0.0.1", "::1"]
        );
    }

    #[test]
    fn witness_is_a_top_level_flag() {
        let c = parse(
            "name = \"w\"\naddress = \"h\"\nwitness = true\n[hypervisor]\nqemu_binary = \"/nonexistent/qemu\"",
        );
        assert!(c.witness);
        c.validate().unwrap();
    }

    #[test]
    fn rejects_unknown_keys_and_missing_address() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\nlisten_addr = \"x\"").is_err()
        );
        assert!(toml::from_str::<Config>("name = \"n\"").is_err());
        let c = parse("name = \"n\"\naddress = \"h\"\n[reconciler]\nacpi_timeout_secs = 5");
        assert_eq!(c.reconciler.acpi_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn validates_name_and_address() {
        let with = |name: &str, address: &str| Config {
            name: name.into(),
            address: address.into(),
            ..parse("name = \"n\"\naddress = \"h\"")
        };
        assert!(with("node1", "10.0.0.5").validate().is_ok());
        assert!(with("node1", "node1.lan").validate().is_ok());
        assert!(with("node1", "::1").validate().is_ok());
        assert!(with("Node_1", "10.0.0.5").validate().is_err());
        for bad in ["", "10.0.0.5:8006", "a b", "[::1]"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
        assert_eq!(with("n", "::1").api_address(), "[::1]:8006");
        for bad in ["0.0.0.0", "::"] {
            assert!(with("node1", bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn migration_listens_on_the_cluster_listen_ip() {
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(c.migration_listen_host(), "0.0.0.0");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"10.0.0.5:8007\"");
        assert_eq!(c.migration_listen_host(), "10.0.0.5");
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\ncluster_listen = \"[::]:8007\"");
        assert_eq!(c.migration_listen_host(), "::");
    }

    #[test]
    fn validates_ports() {
        let ok = parse("name = \"n\"\naddress = \"h\"");
        assert!(ok.validate().is_ok());
        let same = Config {
            cluster_listen: "127.0.0.1:8006".parse().unwrap(),
            ..ok.clone()
        };
        assert!(same.validate().is_err());
        let zero = Config {
            listen: "0.0.0.0:0".parse().unwrap(),
            ..ok.clone()
        };
        assert!(zero.validate().is_err());
        let zero = Config {
            cluster_listen: "0.0.0.0:0".parse().unwrap(),
            ..ok
        };
        assert!(zero.validate().is_err());
    }

    #[test]
    fn schedule_section_shortens_periods_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[schedule]\ntick_secs = 1\nhour_secs = 2");
        assert!(c.validate().is_ok());
        let schedule = c.schedule.scheduler_config();
        assert_eq!(schedule.tick, Duration::from_secs(1));
        assert_eq!(schedule.hour, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "hour_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[schedule]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[schedule]\ntick = 1")
                .is_err()
        );
    }

    #[test]
    fn janitor_section_shortens_periods_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntick_secs = 1\noffline_grace_secs = 2",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.tick, Duration::from_secs(1));
        assert_eq!(janitor.offline_grace, Duration::from_secs(2));
        for bad in ["tick_secs = 0", "offline_grace_secs = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[janitor]\ngrace = 1")
                .is_err()
        );
    }

    #[test]
    fn janitor_section_sets_task_retention_but_not_to_zero() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 2\ntask_max_finished = 50",
        );
        assert!(c.validate().is_ok());
        let janitor = c.janitor.janitor_config();
        assert_eq!(janitor.task_max_age, Duration::from_secs(2 * 24 * 3600));
        assert_eq!(janitor.task_max_finished, 50);
        let c = parse("name = \"n\"\naddress = \"h\"\n[janitor]\ntask_max_age_days = 36500");
        assert!(c.validate().is_ok());
        for bad in [
            "task_max_age_days = 0",
            "task_max_finished = 0",
            "task_max_age_days = 36501",
        ] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[janitor]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn backup_section_sets_the_tick_and_the_limit_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[backup]\ntick_secs = 1\nmax_concurrent = 4");
        assert!(c.validate().is_ok());
        let backup = c.backup.scheduler_config();
        assert_eq!(backup.tick, Duration::from_secs(1));
        assert_eq!(backup.max_concurrent, 4);
        for bad in ["tick_secs = 0", "max_concurrent = 0"] {
            let c = parse(&format!("name = \"n\"\naddress = \"h\"\n[backup]\n{bad}"));
            assert!(c.validate().is_err(), "{bad}");
        }
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[backup]\ntick = 1").is_err()
        );
    }

    #[test]
    fn pools_section_sets_the_refresh_interval_but_not_to_zero() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 2");
        assert!(c.validate().is_ok());
        assert_eq!(c.pools.refresh(), Duration::from_secs(2));
        let c = parse("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh_secs = 0");
        assert!(c.validate().is_err());
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[pools]\nrefresh = 1")
                .is_err()
        );
    }

    #[test]
    fn hypervisor_section_picks_and_overrides_the_profile() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"tcg\"\n\
             qemu_binary = \"/opt/qemu/bin/qemu-system-x86_64\"\nsystemd_scope = false",
        );
        assert!(c.validate().is_ok());
        let p = c.host_profile().unwrap();
        assert_eq!(p.accel, Accel::Tcg);
        assert_eq!(p.qemu_binary, Path::new("/opt/qemu/bin/qemu-system-x86_64"));

        // 다른 OS의 가속기와 macOS의 systemd 스코프는 거부한다.
        let other = if cfg!(target_os = "linux") {
            "hvf"
        } else {
            "kvm"
        };
        let c = parse(&format!(
            "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"{other}\"\nsystemd_scope = false"
        ));
        assert!(c.validate().is_err());
        if !cfg!(target_os = "linux") {
            let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\nsystemd_scope = true");
            assert!(c.validate().is_err());
        }
        assert!(
            toml::from_str::<Config>(
                "name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"xen\""
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_unknown_keys_in_hypervisor_section() {
        assert!(
            toml::from_str::<Config>("name = \"n\"\naddress = \"h\"\n[hypervisor]\nacel = \"kvm\"")
                .is_err()
        );
    }

    #[test]
    fn explicit_kvm_needs_dev_kvm() {
        let c = parse("name = \"n\"\naddress = \"h\"\n[hypervisor]\naccel = \"kvm\"");
        // `/dev/kvm`이 있는 호스트(Linux CI)에서는 KVM을 쓰고, 없으면 오류다.
        match c.host_profile() {
            Ok(p) => {
                assert!(kvm_available());
                assert_eq!(p.accel, Accel::Kvm);
            }
            Err(e) => {
                assert!(!kvm_available());
                assert!(e.to_string().contains("/dev/kvm"), "{e}");
            }
        }
    }

    #[test]
    fn ha_section_defaults_by_os() {
        let c = parse("name = \"n\"\naddress = \"h\"");
        c.validate().unwrap();
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog"));
        assert_eq!(c.ha.watchdog_timeout_secs, 30);
        assert_eq!(c.ha.fence_after_secs, 30);
        assert_eq!(c.ha.fence_after(), Duration::from_secs(30));
        if cfg!(target_os = "linux") {
            assert_eq!(c.ha.fencing, FencingSetting::Watchdog);
            assert_eq!(
                c.ha.node_ha(),
                Some(NodeHa {
                    fencing: Fencing::Watchdog,
                    recover_after_secs: 65,
                })
            );
        } else {
            assert_eq!(c.ha.fencing, FencingSetting::Off);
            assert_eq!(c.ha.node_ha(), None);
        }
    }

    #[test]
    fn ha_section_sets_fencing_and_recover_after() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 2",
        );
        c.validate().unwrap();
        // process는 watchdog 제한 시간을 더하지 않는다.
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Process,
                recover_after_secs: 7,
            })
        );
        let c = parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"");
        c.validate().unwrap();
        assert_eq!(c.ha.node_ha(), None);
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\n\
             watchdog_device = \"/dev/watchdog1\"\nwatchdog_timeout_secs = 10\nfence_after_secs = 20",
        );
        assert_eq!(c.ha.watchdog_device, Path::new("/dev/watchdog1"));
        assert_eq!(
            c.ha.node_ha(),
            Some(NodeHa {
                fencing: Fencing::Watchdog,
                recover_after_secs: 35,
            })
        );
        // watchdog 장치는 Linux에만 있다.
        assert_eq!(c.validate().is_ok(), cfg!(target_os = "linux"));
        for bad in ["fencing = \"ipmi\"", "fence_after = 1"] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[ha]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn ha_section_validates_times() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nfence_after_secs = 0",
        );
        assert!(c.validate().is_err());
        // 꺼져 있어도 fence_after는 검사한다.
        let c =
            parse("name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"off\"\nfence_after_secs = 0");
        assert!(c.validate().is_err());
        // 제한 시간은 5초 이상이다. 장치를 1초마다 쓰다듬으므로 너무 짧으면 쓰다듬기 한 번만 늦어도
        // 노드가 재시작된다. Linux가 아니면 범위 안이어도 장치가 없어 실패한다.
        for (timeout, ok) in [
            (0, false),
            (4, false),
            (5, true),
            (3600, true),
            (3601, false),
        ] {
            let c = parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"watchdog\"\nwatchdog_timeout_secs = {timeout}"
            ));
            match c.validate() {
                Ok(()) => assert!(ok && cfg!(target_os = "linux"), "{timeout}"),
                Err(e) => assert_eq!(
                    e.to_string().contains("watchdog_timeout_secs"),
                    !ok,
                    "{timeout}: {e}"
                ),
            }
        }
        // watchdog가 아니면 장치 제한 시간은 쓰지 않으므로 보지 않는다.
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[ha]\nfencing = \"process\"\nwatchdog_timeout_secs = 0",
        );
        c.validate().unwrap();
    }

    /// 펜싱이 꺼져 있으면 guard가 없고, process는 watchdog 없이, watchdog는 장치와 함께 만든다.
    #[test]
    fn guard_config_follows_the_fencing() {
        let mut ha = HaSection {
            fencing: FencingSetting::Off,
            fence_after_secs: 5,
            ..HaSection::default()
        };
        assert!(ha.guard_config().is_none());
        ha.fencing = FencingSetting::Process;
        let config = ha.guard_config().unwrap();
        assert_eq!(config.fence_after, Duration::from_secs(5));
        assert_eq!(config.renew_interval, Duration::from_millis(500));
        assert!(config.watchdog.is_none());
        ha.fencing = FencingSetting::Watchdog;
        assert!(ha.guard_config().unwrap().watchdog.is_some());
    }

    #[test]
    fn renew_interval_is_a_tenth_of_fence_after_within_bounds() {
        let interval = |fence_after_secs| {
            HaSection {
                fence_after_secs,
                ..HaSection::default()
            }
            .renew_interval()
        };
        assert_eq!(interval(1), Duration::from_millis(200));
        assert_eq!(interval(2), Duration::from_millis(200));
        assert_eq!(interval(5), Duration::from_millis(500));
        assert_eq!(interval(20), Duration::from_secs(2));
        assert_eq!(interval(30), Duration::from_secs(2));
        assert_eq!(interval(u64::MAX), Duration::from_secs(2));
    }

    #[test]
    fn migration_section_parses_ports_and_timeout() {
        let c = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"61000-61009\"\n\
             bandwidth_mib = 100\ntimeout = \"1h30m\"",
        );
        assert_eq!(c.migration.ports, 61000..=61009);
        assert_eq!(c.migration.bandwidth_mib, 100);
        assert_eq!(c.migration.timeout, Duration::from_secs(5400));
        let one = parse(
            "name = \"n\"\naddress = \"h\"\n[migration]\nports = \"60000-60000\"\ntimeout = \"90s\"",
        );
        assert_eq!(one.migration.ports, 60000..=60000);
        assert_eq!(one.migration.timeout, Duration::from_secs(90));
        for bad in [
            "ports = \"60099-60000\"",
            "ports = \"0-10\"",
            "ports = \"60000\"",
            "ports = \"60000-70000\"",
            "ports = \"a-b\"",
            "timeout = \"0s\"",
            "timeout = \"30\"",
            "timeout = \"500ms\"",
            "timeout = \"m\"",
            "timeout = \"\"",
            "bandwidth = 1",
        ] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[migration]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
    }

    #[test]
    fn underlay_comes_from_the_setting_or_the_address() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // 설정이 address보다 앞선다.
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"\n[network]\nunderlay = \"10.1.0.5\"");
        c.validate().unwrap();
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("10.1.0.5")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\n[network]\nunderlay = \"fd00::5\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("fd00::5")));
        // 없으면 address: IPv4·IPv6 리터럴은 그대로, 이름은 푼다.
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.network, NetworkSection::default());
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("10.0.0.5")));
        let c = parse("name = \"n\"\naddress = \"::1\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Ip(ip("::1")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(c.underlay_source(), UnderlaySource::Lookup("node1.lan"));
        for bad in [
            "underlay = \"node1.lan\"",
            "underlay = \"10.0.0.5:4789\"",
            "mtu = 1450",
        ] {
            let text = format!("name = \"n\"\naddress = \"h\"\n[network]\n{bad}");
            assert!(toml::from_str::<Config>(&text).is_err(), "{bad}");
        }
        for bad in ["0.0.0.0", "::"] {
            let c = parse(&format!(
                "name = \"n\"\naddress = \"h\"\n[network]\nunderlay = \"{bad}\""
            ));
            let err = c.validate().unwrap_err().to_string();
            assert!(err.contains("network.underlay"), "{err}");
        }
    }

    #[test]
    fn resolved_underlay_skips_local_addresses_and_prefers_ipv4() {
        let pick = |addrs: &[&str]| pick_underlay(addrs.iter().map(|a| a.parse().unwrap()));
        // `/etc/hosts`의 127.0.1.1 같은 loopback, link-local, 0.0.0.0은 다른 노드가 닿을 수 없다.
        assert_eq!(pick(&["127.0.1.1"]), None);
        assert_eq!(pick(&["169.254.1.1", "fe80::1", "::", "0.0.0.0"]), None);
        assert_eq!(pick(&[]), None);
        assert_eq!(
            pick(&["::1", "10.0.0.5"]),
            Some("10.0.0.5".parse().unwrap())
        );
        assert_eq!(
            pick(&["fe80::1", "2001:db8::5"]),
            Some("2001:db8::5".parse().unwrap())
        );
        // IPv4가 먼저다.
        assert_eq!(
            pick(&["2001:db8::5", "10.0.0.5", "10.0.0.6"]),
            Some("10.0.0.5".parse().unwrap())
        );
    }

    #[tokio::test]
    async fn underlay_resolves_a_name_or_is_none() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        // 이름 풀기를 바꿔 끼워 DNS에 기대지 않는다.
        let lookup = |result: std::io::Result<Vec<IpAddr>>| {
            move |host: String| {
                assert_eq!(host, "node1.lan");
                std::future::ready(result)
            }
        };
        let no_lookup =
            |_: String| -> std::future::Ready<std::io::Result<Vec<IpAddr>>> { panic!("lookup") };
        let c = parse("name = \"n\"\naddress = \"10.0.0.5\"");
        assert_eq!(c.underlay().await, Some(ip("10.0.0.5")));
        // 리터럴과 설정은 풀지 않는다. 리터럴은 loopback이어도 그대로 쓴다.
        let c = parse("name = \"n\"\naddress = \"127.0.0.1\"");
        assert_eq!(c.underlay_with(no_lookup).await, Some(ip("127.0.0.1")));
        let c = parse("name = \"n\"\naddress = \"node1.lan\"\n[network]\nunderlay = \"10.1.0.5\"");
        assert_eq!(c.underlay_with(no_lookup).await, Some(ip("10.1.0.5")));
        // 이름은 풀어서 쓸 수 있는 주소를 고른다. loopback만 나오거나 풀지 못하면 None이다.
        let c = parse("name = \"n\"\naddress = \"node1.lan\"");
        assert_eq!(
            c.underlay_with(lookup(Ok(vec![ip("127.0.1.1"), ip("10.0.0.7")])))
                .await,
            Some(ip("10.0.0.7"))
        );
        assert_eq!(
            c.underlay_with(lookup(Ok(vec![ip("127.0.1.1")]))).await,
            None
        );
        assert_eq!(
            c.underlay_with(lookup(Err(std::io::Error::other("no such host"))))
                .await,
            None
        );
    }
}
```

`crates/opld/src/member.rs`에서

```rust
use opl_api::{
    AppState, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig, IncomingMigrations,
    SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
};
```

을 다음으로 바꾼다:

```rust
use opl_api::{
    AppState, BackupJobScheduler, ClusterJanitor, HaManager, HaManagerConfig, IncomingConfig,
    IncomingMigrations, SnapshotScheduler, TaskRunner, TaskRunnerConfig, UefiVarsFetcher,
};
```

`crates/opld/src/member.rs`에서

```rust
    janitor: ClusterJanitor,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
```

을 다음으로 바꾼다:

```rust
    janitor: ClusterJanitor,
    /// 리더가 백업 작업을 실행한다.
    backups: BackupJobScheduler,
    /// 리더가 리스가 끝난 노드의 HA VM을 옮긴다(이 노드의 펜싱 설정과 상관없다).
```

`crates/opld/src/member.rs`에서

```rust
        self.janitor.shutdown().await;
        self.recovery.shutdown().await;
```

을 다음으로 바꾼다:

```rust
        self.janitor.shutdown().await;
        self.backups.shutdown().await;
        self.recovery.shutdown().await;
```

`crates/opld/src/member.rs`에서

```rust
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

을 다음으로 바꾼다:

```rust
        let janitor = ClusterJanitor::spawn(state.clone(), config.janitor.janitor_config());
        let backups = BackupJobScheduler::spawn(state.clone(), config.backup.scheduler_config());
        let recovery = HaManager::spawn(state.clone(), HaManagerConfig::default());
```

`crates/opld/src/member.rs`에서

```rust
                janitor,
                recovery,
```

을 다음으로 바꾼다:

```rust
                janitor,
                backups,
                recovery,
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1007개, 실패 0

Run: `cargo test -p opl-api --features storage-it --test backup_job_runs`
Expected: PASS (qemu-img가 필요)

- [ ] **Step 3: 커밋**

```bash
git add crates
git commit -m "feat(opl-api): 백업 작업 스케줄러 — 리더가 실행 시작·VM마다 백업 요청(노드당 동시 제한)·끝나면 last_run과 보존 규칙으로 지우기, opld [backup] 설정"
```

---

### Task 4: oplctl·웹에서 백업 작업

oplctl `backup-job create NAME --pool P (--all | --vm NAME... | --label K=V...) --at HH:MM [--days mon,wed] [--keep-last/daily/weekly/monthly N] [--disabled]`, `list`(`NAME POOL VMS SCHEDULE KEEP ENABLED NEXT LAST`), `get`, `set`(지금 작업과 합쳐 PATCH), `delete`, `run`. 웹: 백업 작업 페이지(목록·만들기·편집·지우기·지금 실행, 다음·마지막 실행은 UTC, 오류), 사이드바, 백업 목록의 작업 열, 오류 문구, en·ko.

**Files:**
- Create: `web/src/backups/BackupJobsPage.test.tsx`
- Create: `web/src/backups/BackupJobsPage.tsx`
- Create: `web/src/backups/jobs.test.ts`
- Create: `web/src/backups/jobs.ts`
- Modify: `crates/oplctl/src/cli.rs`
- Modify: `web/src/App.test.tsx`
- Modify: `web/src/App.tsx`
- Modify: `web/src/api/LiveProvider.test.tsx`
- Modify: `web/src/api/live.ts`
- Modify: `web/src/api/types.ts`
- Modify: `web/src/auth/permissions.test.ts`
- Modify: `web/src/auth/permissions.ts`
- Modify: `web/src/backups/BackupTable.tsx`
- Modify: `web/src/backups/BackupsPage.test.tsx`
- Modify: `web/src/i18n/en.json`
- Modify: `web/src/i18n/i18n.test.ts`
- Modify: `web/src/i18n/ko.json`
- Modify: `web/src/layout/AppLayout.test.tsx`
- Modify: `web/src/layout/AppLayout.tsx`
- Modify: `web/src/test/fixtures.ts`
- Modify: `web/src/test/render.tsx`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`crates/oplctl/src/cli.rs`에서

```rust
        cmd: BackupCmd,
    },
```

을 다음으로 바꾼다:

```rust
        cmd: BackupCmd,
    },
    /// 백업 작업: 정한 시각(UTC)에 고른 VM들을 백업하고 보존 규칙대로 옛 백업을 지운다.
    BackupJob {
        #[command(subcommand)]
        cmd: BackupJobCmd,
    },
```

`crates/oplctl/src/cli.rs`에서

```rust
#[derive(Subcommand)]
enum TaskCmd {
```

을 다음으로 바꾼다:

```rust
#[derive(Subcommand)]
enum BackupJobCmd {
    /// 백업 작업을 만든다. 정한 요일의 시각(UTC)에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛
    /// 백업을 지운다.
    Create(BackupJobCreate),
    List,
    Get {
        job: String,
    },
    /// 준 값만 바꾼다. 일정이나 보존 규칙의 일부만 주면 나머지는 지금 값을 그대로 둔다.
    Set(BackupJobSet),
    /// 작업만 지운다. 작업이 만든 백업은 남는다.
    Delete {
        job: String,
    },
    /// 지금 실행한다. 리더의 스케줄러가 다음 차례에 시작하며 끝나기를 기다리지 않는다.
    Run {
        job: String,
    },
}

/// 백업할 VM 고르기. 셋 중 하나만 준다.
#[derive(Args)]
struct VmsArgs {
    /// 모든 VM
    #[arg(long)]
    all: bool,
    /// 이 이름의 VM(여러 번 줄 수 있다)
    #[arg(long = "vm", value_name = "NAME")]
    vms: Vec<String>,
    /// 이 라벨을 모두 가진 VM(여러 번 줄 수 있다)
    #[arg(long = "label", value_name = "KEY=VALUE", value_parser = parse_label)]
    labels: Vec<(String, String)>,
}

/// 보존 규칙. 모두 0이면 지우지 않는다.
#[derive(Args)]
struct KeepArgs {
    /// 최신 N개를 남긴다.
    #[arg(long, value_name = "N")]
    keep_last: Option<u32>,
    /// 그다음 하루(UTC)마다 가장 새 것 하나씩 N일
    #[arg(long, value_name = "N")]
    keep_daily: Option<u32>,
    /// 그다음 ISO 주마다 가장 새 것 하나씩 N주
    #[arg(long, value_name = "N")]
    keep_weekly: Option<u32>,
    /// 그다음 달마다 가장 새 것 하나씩 N달
    #[arg(long, value_name = "N")]
    keep_monthly: Option<u32>,
}

impl KeepArgs {
    fn given(&self) -> bool {
        self.keep_last.is_some()
            || self.keep_daily.is_some()
            || self.keep_weekly.is_some()
            || self.keep_monthly.is_some()
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum Day {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").required(true).args(["all", "vms", "labels"])))]
struct BackupJobCreate {
    name: String,
    /// 백업 풀(`pool set NAME --backups true`로 표시한 풀)
    #[arg(long)]
    pool: String,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: String,
    /// 실행할 요일(쉼표로 나눔). 생략하면 매일.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 꺼 둔 채로 만든다.
    #[arg(long)]
    disabled: bool,
}

#[derive(Args)]
#[command(group(ArgGroup::new("selector").args(["all", "vms", "labels"])))]
#[command(group(ArgGroup::new("change").required(true).multiple(true).args([
    "all", "vms", "labels", "pool", "at", "days",
    "keep_last", "keep_daily", "keep_weekly", "keep_monthly", "enabled",
])))]
struct BackupJobSet {
    job: String,
    #[arg(long)]
    pool: Option<String>,
    #[command(flatten)]
    vms: VmsArgs,
    /// 실행 시각(UTC)
    #[arg(long, value_name = "HH:MM")]
    at: Option<String>,
    /// 실행할 요일(쉼표로 나눔). 모든 요일을 주면 매일이다.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "mon,tue,...")]
    days: Vec<Day>,
    #[command(flatten)]
    keep: KeepArgs,
    /// 예약 실행을 켜거나(다음 실행 시각을 지금부터 다시 정한다) 끈다. 진행 중인 실행은 끝까지 간다.
    #[arg(long, value_name = "true|false")]
    enabled: Option<bool>,
}

/// `--label`의 `KEY=VALUE`. 값에는 `=`가 있어도 된다.
fn parse_label(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((key, value)) if !key.is_empty() => Ok((key.into(), value.into())),
        _ => Err(format!("invalid label {s:?}: use KEY=VALUE")),
    }
}

/// VM 고르기(`vms` 값). 주지 않았으면 `None`이다.
fn vms_value(args: &VmsArgs) -> Option<Value> {
    if args.all {
        Some(json!({ "all": true }))
    } else if !args.vms.is_empty() {
        Some(json!({ "names": args.vms }))
    } else if !args.labels.is_empty() {
        let labels: serde_json::Map<String, Value> = args
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        Some(json!({ "labels": labels }))
    } else {
        None
    }
}

fn days_value(days: &[Day]) -> Value {
    json!(days.iter().map(|d| value_name(*d)).collect::<Vec<_>>())
}

/// 보존 규칙(`keep` 값). 주지 않은 단위는 `current`의 값, 거기도 없으면 0이다.
fn keep_value(args: &KeepArgs, current: &Value) -> Value {
    let pick = |arg: Option<u32>, field: &str| {
        arg.map_or_else(|| current[field].as_u64().unwrap_or(0), u64::from)
    };
    json!({
        "last": pick(args.keep_last, "last"),
        "daily": pick(args.keep_daily, "daily"),
        "weekly": pick(args.keep_weekly, "weekly"),
        "monthly": pick(args.keep_monthly, "monthly"),
    })
}

/// `backup-job create`의 요청 본문
fn backup_job_create_body(args: &BackupJobCreate) -> Value {
    json!({
        "name": args.name,
        "spec": {
            "vms": vms_value(&args.vms),
            "pool": args.pool,
            "schedule": { "at": args.at, "days": days_value(&args.days) },
            "keep": keep_value(&args.keep, &Value::Null),
            "enabled": !args.disabled,
        }
    })
}

/// `backup-job set`의 요청 본문(`PATCH /backup-jobs/{key}`). 준 값만 보낸다. 서버는 일정과 보존
/// 규칙을 통째로 바꾸므로 그 일부만 주었으면 나머지는 읽은 작업(`current`)의 값으로 채운다.
fn backup_job_set_body(args: &BackupJobSet, current: &Value) -> Value {
    let mut body = json!({});
    if let Some(vms) = vms_value(&args.vms) {
        body["vms"] = vms;
    }
    if let Some(pool) = &args.pool {
        body["pool"] = json!(pool);
    }
    let schedule = &current["spec"]["schedule"];
    if args.at.is_some() || !args.days.is_empty() {
        body["schedule"] = json!({
            "at": args.at.as_ref().map_or_else(|| schedule["at"].clone(), |at| json!(at)),
            "days": if args.days.is_empty() {
                schedule["days"].as_array().map_or_else(|| json!([]), |d| json!(d))
            } else {
                days_value(&args.days)
            },
        });
    }
    if args.keep.given() {
        body["keep"] = keep_value(&args.keep, &current["spec"]["keep"]);
    }
    if let Some(enabled) = args.enabled {
        body["enabled"] = json!(enabled);
    }
    body
}

#[derive(Subcommand)]
enum TaskCmd {
```

`crates/oplctl/src/cli.rs`에서

```rust
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
```

을 다음으로 바꾼다:

```rust
        Cmd::Backup { cmd } => backup(&client, cmd, json_out, out).await?,
        Cmd::BackupJob { cmd } => backup_job(&client, cmd, json_out, out).await?,
        Cmd::Task { cmd } => match cmd {
```

`crates/oplctl/src/cli.rs`에서

```rust
        size,
    ]
```

을 다음으로 바꾼다:

```rust
        size,
    ]
}

async fn backup_job(
    client: &Client,
    cmd: BackupJobCmd,
    json_out: bool,
    out: &mut (dyn Write + Send),
) -> anyhow::Result<()> {
    match cmd {
        BackupJobCmd::Create(args) => {
            let job = client
                .post("/backup-jobs", &backup_job_create_body(&args))
                .await?;
            if args.disabled {
                writeln!(out, "backup job {} created (disabled)", args.name)?;
            } else {
                let next = job["status"]["next_run"]
                    .as_i64()
                    .map_or_else(|| "-".to_string(), unix_time);
                writeln!(out, "backup job {} created; next run {next}", args.name)?;
            }
        }
        BackupJobCmd::List => {
            let list = client.get("/backup-jobs").await?;
            print_list(out, json_out, &list, BACKUP_JOB_HEADERS, backup_job_row)?;
        }
        BackupJobCmd::Get { job } => {
            print_json(out, &client.get(&format!("/backup-jobs/{job}")).await?)?
        }
        BackupJobCmd::Set(args) => {
            // If-Match 없이 보낸다: 스케줄러가 status를 자주 써서 revision이 곧잘 바뀐다.
            let path = format!("/backup-jobs/{}", args.job);
            let current = client.get(&path).await?;
            client
                .patch(&path, &backup_job_set_body(&args, &current))
                .await?;
            writeln!(out, "backup job {} updated", args.job)?;
        }
        BackupJobCmd::Delete { job } => {
            client.delete(&format!("/backup-jobs/{job}")).await?;
            writeln!(out, "backup job {job} deleted; its backups are kept")?;
        }
        BackupJobCmd::Run { job } => {
            client
                .send(client.request(Method::POST, &format!("/backup-jobs/{job}/run")))
                .await?;
            writeln!(
                out,
                "backup job {job} will run at the scheduler's next check"
            )?;
        }
    }
    Ok(())
}

/// `backup-job list`의 열
const BACKUP_JOB_HEADERS: &[&str] = &[
    "NAME", "POOL", "VMS", "SCHEDULE", "KEEP", "ENABLED", "NEXT", "LAST",
];

/// `backup-job list`의 한 행. 시각은 모두 UTC다. 끈 작업은 다음 실행이 없다.
fn backup_job_row(j: &Value) -> Vec<String> {
    let spec = &j["spec"];
    let status = &j["status"];
    let vms = &spec["vms"];
    let vms = if vms["all"] == true {
        "all".to_string()
    } else if let Some(names) = vms["names"].as_array() {
        names.iter().map(cell).collect::<Vec<_>>().join(",")
    } else if let Some(labels) = vms["labels"].as_object() {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={}", cell(v)))
            .collect::<Vec<_>>()
            .join(",")
    } else {
        "-".to_string()
    };
    let days = match spec["schedule"]["days"].as_array() {
        Some(days) if !days.is_empty() => days.iter().map(cell).collect::<Vec<_>>().join(","),
        _ => "daily".to_string(),
    };
    let schedule = format!("{days} {} UTC", cell(&spec["schedule"]["at"]));
    let keep: Vec<String> = ["last", "daily", "weekly", "monthly"]
        .iter()
        .filter_map(|unit| match spec["keep"][unit].as_u64() {
            Some(n) if n > 0 => Some(format!("{unit}={n}")),
            _ => None,
        })
        .collect();
    let keep = if keep.is_empty() {
        "all".to_string()
    } else {
        keep.join(",")
    };
    let enabled = spec["enabled"] != false;
    let next = match status["next_run"].as_i64() {
        Some(t) if enabled => unix_time(t),
        _ => "-".to_string(),
    };
    let last = if !status["run"].is_null() {
        "running".to_string()
    } else {
        let r = &status["last_run"];
        match r["finished_at"].as_i64() {
            Some(t) => format!(
                "{} ({} ok, {} failed)",
                unix_time(t),
                cell(&r["ok"]),
                cell(&r["failed"])
            ),
            None => "-".to_string(),
        }
    };
    vec![
        cell(&j["name"]),
        cell(&spec["pool"]),
        vms,
        schedule,
        keep,
        enabled.to_string(),
        next,
        last,
    ]
```

`crates/oplctl/src/cli.rs`에서

```rust
    }

    #[test]
    fn parses_pool_create_commands() {
```

을 다음으로 바꾼다:

```rust
    }

    /// `backup-job create`의 인자를 요청 본문으로
    fn backup_job_create(args: &[&str]) -> Result<Value, clap::Error> {
        let mut argv = vec!["oplctl", "backup-job", "create", "nightly"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.cmd {
            Cmd::BackupJob {
                cmd: BackupJobCmd::Create(c),
            } => backup_job_create_body(&c),
            _ => unreachable!(),
        })
    }

    #[test]
    fn backup_job_create_builds_the_spec() {
        assert_eq!(
            backup_job_create(&[
                "--pool",
                "bk",
                "--label",
                "tier=prod",
                "--label",
                "env=a=b",
                "--at",
                "03:00",
                "--days",
                "mon,wed",
                "--keep-last",
                "2",
                "--keep-weekly",
                "4",
            ])
            .unwrap(),
            json!({
                "name": "nightly",
                "spec": {
                    "vms": { "labels": { "tier": "prod", "env": "a=b" } },
                    "pool": "bk",
                    "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                    "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                    "enabled": true
                }
            })
        );
        // 요일을 주지 않으면 매일, 보존 규칙을 주지 않으면 모두 0(지우지 않음)
        let all =
            backup_job_create(&["--pool", "bk", "--all", "--at", "00:00", "--disabled"]).unwrap();
        assert_eq!(all["spec"]["vms"], json!({ "all": true }));
        assert_eq!(
            all["spec"]["schedule"],
            json!({ "at": "00:00", "days": [] })
        );
        assert_eq!(
            all["spec"]["keep"],
            json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 })
        );
        assert_eq!(all["spec"]["enabled"], json!(false));
        assert_eq!(
            backup_job_create(&["--pool", "bk", "--vm", "web", "--vm", "db", "--at", "03:00"])
                .unwrap()["spec"]["vms"],
            json!({ "names": ["web", "db"] })
        );
        // VM 고르기는 셋 중 꼭 하나, 풀과 시각은 꼭, 라벨은 KEY=VALUE, 요일은 mon..sun
        for args in [
            &["--pool", "bk", "--at", "03:00"][..],
            &["--pool", "bk", "--all", "--vm", "web", "--at", "03:00"],
            &[
                "--pool", "bk", "--vm", "web", "--label", "a=b", "--at", "03:00",
            ],
            &["--all", "--at", "03:00"],
            &["--pool", "bk", "--all"],
            &["--pool", "bk", "--label", "tier", "--at", "03:00"],
            &["--pool", "bk", "--label", "=prod", "--at", "03:00"],
            &["--pool", "bk", "--all", "--at", "03:00", "--days", "monday"],
            &[
                "--pool",
                "bk",
                "--all",
                "--at",
                "03:00",
                "--keep-last",
                "-1",
            ],
        ] {
            assert!(backup_job_create(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_set_changes_only_what_is_given() {
        let current = json!({
            "name": "nightly",
            "revision": 9,
            "spec": {
                "vms": { "all": true }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["sat"] },
                "keep": { "last": 2, "daily": 7, "weekly": 0, "monthly": 0 },
                "enabled": true
            }
        });
        let set = |args: &[&str]| {
            let mut argv = vec!["oplctl", "backup-job", "set", "nightly"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|cli| match cli.cmd {
                Cmd::BackupJob {
                    cmd: BackupJobCmd::Set(s),
                } => backup_job_set_body(&s, &current),
                _ => unreachable!(),
            })
        };
        assert_eq!(
            set(&["--enabled", "false"]).unwrap(),
            json!({ "enabled": false })
        );
        assert_eq!(
            set(&["--vm", "web", "--pool", "bk2"]).unwrap(),
            json!({ "vms": { "names": ["web"] }, "pool": "bk2" })
        );
        // 일정과 보존 규칙은 통째로 바뀌므로 주지 않은 부분은 지금 값을 보낸다.
        assert_eq!(
            set(&["--at", "04:30"]).unwrap(),
            json!({ "schedule": { "at": "04:30", "days": ["sat"] } })
        );
        assert_eq!(
            set(&["--days", "sun,mon"]).unwrap(),
            json!({ "schedule": { "at": "03:00", "days": ["sun", "mon"] } })
        );
        assert_eq!(
            set(&["--keep-monthly", "6", "--keep-last", "1"]).unwrap(),
            json!({ "keep": { "last": 1, "daily": 7, "weekly": 0, "monthly": 6 } })
        );
        // 바꿀 것은 꼭 하나, VM 고르기는 하나만, 사용 여부는 true나 false
        for args in [
            &[][..],
            &["--all", "--vm", "web"],
            &["--enabled"],
            &["--enabled", "yes"],
        ] {
            assert!(set(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn backup_job_list_shows_the_schedule_and_runs() {
        let job = json!({
            "name": "nightly",
            "spec": {
                "vms": { "labels": { "env": "a", "tier": "prod" } }, "pool": "bk",
                "schedule": { "at": "03:00", "days": ["mon", "wed"] },
                "keep": { "last": 2, "daily": 0, "weekly": 4, "monthly": 0 },
                "enabled": true
            },
            "status": {
                "next_run": 1_793_761_200, "run": null,
                "last_run": { "started_at": 1_793_674_800, "finished_at": 1_793_675_100, "ok": 3, "failed": 1 },
                "errors": [{ "vm": "db", "error": "boom" }]
            }
        });
        assert_eq!(
            backup_job_row(&job),
            [
                "nightly",
                "bk",
                "env=a,tier=prod",
                "mon,wed 03:00 UTC",
                "last=2,weekly=4",
                "true",
                "2026-11-04 03:00 UTC",
                "2026-11-03 03:05 UTC (3 ok, 1 failed)"
            ]
        );
        // 매일, 보존 규칙 없음, 진행 중
        let mut running = job.clone();
        running["spec"]["vms"] = json!({ "names": ["web", "db"] });
        running["spec"]["schedule"]["days"] = json!([]);
        running["spec"]["keep"] = json!({ "last": 0, "daily": 0, "weekly": 0, "monthly": 0 });
        running["status"]["run"] = json!({ "started_at": 1, "queue": [], "backups": [] });
        assert_eq!(
            backup_job_row(&running)[2..],
            [
                "web,db",
                "daily 03:00 UTC",
                "all",
                "true",
                "2026-11-04 03:00 UTC",
                "running"
            ]
        );
        // 끈 작업은 다음 실행이 없고, 한 번도 돌지 않았으면 마지막 실행도 없다.
        let mut off = job.clone();
        off["spec"]["vms"] = json!({ "all": true });
        off["spec"]["enabled"] = json!(false);
        off["status"]["last_run"] = Value::Null;
        assert_eq!(backup_job_row(&off)[2], "all");
        assert_eq!(backup_job_row(&off)[5..], ["false", "-", "-"]);
    }

    #[test]
    fn parses_pool_create_commands() {
```

`crates/oplctl/src/cli.rs`에서

```rust
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
    }
```

을 다음으로 바꾼다:

```rust
        assert_eq!(String::from_utf8(out).unwrap(), "backup b1 deleted\n");
    }

    /// `backup-job set`은 작업을 읽어 그 값에 맞춰 PATCH하고, `run`은 본문 없이 POST한다.
    #[tokio::test]
    async fn backup_job_set_reads_then_patches_and_run_posts() {
        let (client, seen) = fake_api(|_| {
            json!({
                "name": "nightly", "revision": 3,
                "spec": { "schedule": { "at": "03:00", "days": [] }, "keep": {} },
                "status": {}
            })
        })
        .await;
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "set", "nightly", "--at", "04:00"])
            .unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        let mut out = Vec::new();
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        let cli = Cli::try_parse_from(["oplctl", "backup-job", "run", "nightly"]).unwrap();
        let Cmd::BackupJob { cmd } = cli.cmd else {
            unreachable!()
        };
        backup_job(&client, cmd, false, &mut out).await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "GET /api/v1/backup-jobs/nightly HTTP/1.1",
                "PATCH /api/v1/backup-jobs/nightly HTTP/1.1",
                "POST /api/v1/backup-jobs/nightly/run HTTP/1.1"
            ]
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "backup job nightly updated\nbackup job nightly will run at the scheduler's next check\n"
        );
    }
```

`web/src/App.test.tsx`에서

```tsx
      "/backups",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

을 다음으로 바꾼다:

```tsx
      "/backups",
      "/backup-jobs",
    ].map((path) => http.get(apiUrl(path), () => HttpResponse.json({ revision: 1, items: [] }))),
```

`web/src/App.tsx`에서

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupsPage } from "@/backups/BackupsPage";
```

을 다음으로 바꾼다:

```tsx
import { RequireAuth } from "@/auth/RequireAuth";
import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { BackupsPage } from "@/backups/BackupsPage";
```

`web/src/App.tsx`에서

```tsx
            { path: "backups", element: <BackupsPage /> },
            { path: "networks", element: <NetworksPage /> },
```

을 다음으로 바꾼다:

```tsx
            { path: "backups", element: <BackupsPage /> },
            { path: "backup-jobs", element: <BackupJobsPage /> },
            { path: "networks", element: <NetworksPage /> },
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
    http.get(apiUrl("/backups"), empty),
  ];
```

을 다음으로 바꾼다:

```tsx
    http.get(apiUrl("/backups"), empty),
    http.get(apiUrl("/backup-jobs"), empty),
  ];
```

`web/src/api/LiveProvider.test.tsx`에서

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup&since=7",
  );
```

을 다음으로 바꾼다:

```tsx
  expect(urls[0]).toContain(
    "kinds=vm,volume,storage-pool,node,task,network,snapshot,security-group,backup,backup-job&since=7",
  );
```

`web/src/api/live.ts`에서

```ts
  "backup",
];
```

을 다음으로 바꾼다:

```ts
  "backup",
  "backup-job",
];
```

`web/src/api/live.ts`에서

```ts
        return call(api.GET("/backups"));
    }
```

을 다음으로 바꾼다:

```ts
        return call(api.GET("/backups"));
      case "backup-job":
        return call(api.GET("/backup-jobs"));
    }
```

`web/src/api/types.ts`에서

```ts
export type Backup = Schemas["Resource_BackupSpec_BackupStatus"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
```

을 다음으로 바꾼다:

```ts
export type Backup = Schemas["Resource_BackupSpec_BackupStatus"];
export type BackupJob = Schemas["Resource_BackupJobSpec_BackupJobStatus"];
export type Weekday = Schemas["Weekday"];
export type SecurityGroup = Schemas["Resource_SecurityGroupSpec_SecurityGroupStatus"];
```

`web/src/api/types.ts`에서

```ts
  | "security-group"
  | "backup";
```

을 다음으로 바꾼다:

```ts
  | "security-group"
  | "backup"
  | "backup-job";
```

`web/src/api/types.ts`에서

```ts
  backup: Backup;
}
```

을 다음으로 바꾼다:

```ts
  backup: Backup;
  "backup-job": BackupJob;
}
```

`web/src/auth/permissions.test.ts`에서

```ts
  expect(can("operator", "write", "backup")).toBe(true);
});
```

을 다음으로 바꾼다:

```ts
  expect(can("operator", "write", "backup")).toBe(true);
  // 백업 작업도 백업과 같다.
  expect(can("viewer", "read", "backup-job")).toBe(true);
  expect(can("viewer", "write", "backup-job")).toBe(false);
  expect(can("operator", "write", "backup-job")).toBe(true);
});
```

`web/src/auth/permissions.ts`에서

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨, 백업만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

을 다음으로 바꾼다:

```ts
 * 서버의 `RoleAuthorizer`와 같은 규칙으로 버튼과 메뉴를 숨긴다. 실제 권한 확인은 서버가 한다.
 * operator는 VM과 볼륨, 백업과 백업 작업만 바꾸고, viewer는 읽기만 한다. 둘 다 계정은 읽지 못한다.
 */
```

`web/src/auth/permissions.ts`에서

```ts
    case "operator":
      return action === "read" ? !account : kind === "vm" || kind === "volume" || kind === "backup";
    case "viewer":
```

을 다음으로 바꾼다:

```ts
    case "operator":
      return action === "read"
        ? !account
        : kind === "vm" || kind === "volume" || kind === "backup" || kind === "backup-job";
    case "viewer":
```

`web/src/backups/BackupJobsPage.test.tsx`를 만든다:

```tsx
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { http, HttpResponse } from "msw";

import { BackupJobsPage } from "@/backups/BackupJobsPage";
import { backupJob, pool } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
import { apiUrl, server } from "@/test/server";

const nightly = backupJob("nightly", 1, {
  spec: {
    vms: { labels: { tier: "prod" } },
    schedule: { at: "03:00", days: ["mon", "wed"] },
    keep: { last: 2, daily: 0, weekly: 4, monthly: 0 },
  },
  status: {
    last_run: { started_at: 1_793_674_800, finished_at: 1_793_675_100, ok: 3, failed: 1 },
    errors: [{ vm: "db", error: "a backup of this VM is already running" }],
  },
});
const running = backupJob("hourly", 2, {
  spec: { vms: { names: ["web", "db"] }, keep: { last: 0, daily: 0, weekly: 0, monthly: 0 } },
  status: { run: { started_at: 1, queue: ["x", "y"], backups: [] } },
});
const off = backupJob("weekly", 3, { spec: { enabled: false } });

function cells(table: HTMLElement, name: string) {
  const headers = within(table).getAllByRole("columnheader");
  const row = within(table).getByText(name).closest("tr")!;
  return (header: string) =>
    within(row).getAllByRole("cell")[headers.findIndex((h) => h.textContent === header)];
}

it("lists jobs with their schedule, retention and runs in UTC", async () => {
  mockLists({ "backup-job": [off, running, nightly] });
  renderPage(<BackupJobsPage />);
  const table = await screen.findByRole("table", { name: "Backup jobs" });
  await within(table).findByText("nightly");
  const job = cells(table, "nightly");
  expect(job("VMs")).toHaveTextContent("tier=prod");
  expect(job("Schedule (UTC)")).toHaveTextContent("Mon, Wed 03:00");
  expect(job("Keep")).toHaveTextContent("last 2, 4 weekly");
  expect(job("State")).toHaveTextContent("On");
  expect(job("Next run")).toHaveTextContent("2026-11-04 03:00 UTC");
  expect(job("Last run")).toHaveTextContent("2026-11-03 03:05 UTC3 ok, 1 failed");
  expect(job("Last run")).toHaveTextContent("db: a backup of this VM is already running");

  const hourly = cells(table, "hourly");
  expect(hourly("VMs")).toHaveTextContent("web, db");
  expect(hourly("Schedule (UTC)")).toHaveTextContent("Every day 03:00");
  expect(hourly("Keep")).toHaveTextContent("All backups");
  expect(hourly("State")).toHaveTextContent("Running2 VMs waiting");
  expect(hourly("Last run")).toHaveTextContent("—");
  // 진행 중이거나 꺼 둔 작업은 지금 실행할 수 없다.
  expect(screen.getByRole("button", { name: "Run hourly now" })).toBeDisabled();

  const weekly = cells(table, "weekly");
  expect(weekly("VMs")).toHaveTextContent("All VMs");
  expect(weekly("State")).toHaveTextContent("Off");
  expect(weekly("Next run")).toHaveTextContent("—");
  expect(screen.getByRole("button", { name: "Run weekly now" })).toBeDisabled();
  // 이름 순
  expect(
    within(table)
      .getAllByRole("row")
      .map((r) => r.firstChild?.textContent),
  ).toEqual(["Name", "hourly", "nightly", "weekly"]);
});

it("creates a job after checking the form", async () => {
  mockLists({
    "backup-job": [nightly],
    "storage-pool": [pool("local"), pool("bk", { backups: true }), pool("bk2", { backups: true })],
  });
  let body: unknown;
  server.use(
    http.post(apiUrl("/backup-jobs"), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(backupJob("prod", 9), { status: 201 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Create" }));
  const dialog = await screen.findByRole("dialog", { name: "Create backup job" });
  // 백업 풀만 고를 수 있다.
  expect(
    within(within(dialog).getByLabelText("Backup pool"))
      .getAllByRole("option")
      .map((o) => o.textContent),
  ).toEqual(["bk", "bk2"]);
  await userEvent.type(within(dialog).getByLabelText("Name"), "nightly");
  await userEvent.selectOptions(within(dialog).getByLabelText("VMs to back up"), "labels");
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "25:00");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(within(dialog).getByText("A backup job with this name already exists.")).toBeVisible();
  expect(within(dialog).getByText("Enter at least one KEY=VALUE label.")).toBeVisible();
  expect(within(dialog).getByText("Enter a time from 00:00 to 23:59.")).toBeVisible();

  await userEvent.clear(within(dialog).getByLabelText("Name"));
  await userEvent.type(within(dialog).getByLabelText("Name"), "prod");
  await userEvent.selectOptions(within(dialog).getByLabelText("Backup pool"), "bk2");
  await userEvent.type(within(dialog).getByLabelText("Labels"), "tier=prod");
  await userEvent.clear(within(dialog).getByLabelText("Time (UTC)"));
  await userEvent.type(within(dialog).getByLabelText("Time (UTC)"), "01:30");
  await userEvent.click(within(dialog).getByLabelText("Sat"));
  await userEvent.type(within(dialog).getByLabelText("Last"), "2");
  await userEvent.type(within(dialog).getByLabelText("Monthly"), "6");
  await userEvent.click(within(dialog).getByRole("button", { name: "Create" }));
  expect(await screen.findByText("Backup job prod created.")).toBeInTheDocument();
  expect(body).toEqual({
    name: "prod",
    spec: {
      vms: { labels: { tier: "prod" } },
      pool: "bk2",
      schedule: { at: "01:30", days: ["sat"] },
      keep: { last: 2, daily: 0, weekly: 0, monthly: 6 },
      enabled: true,
    },
  });
});

it("edits a job and sends only what changed", async () => {
  mockLists({ "backup-job": [nightly], "storage-pool": [pool("bk", { backups: true })] });
  let body: unknown;
  server.use(
    http.patch(apiUrl(`/backup-jobs/${nightly.id}`), async ({ request }) => {
      body = await request.json();
      return HttpResponse.json(nightly);
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Edit nightly" }));
  const dialog = await screen.findByRole("dialog", { name: "Edit nightly" });
  expect(within(dialog).getByLabelText("Labels")).toHaveValue("tier=prod");
  expect(within(dialog).getByLabelText("Mon")).toBeChecked();
  expect(within(dialog).getByLabelText("Weekly")).toHaveValue("4");
  // 바꾼 것이 없으면 저장하지 않는다.
  expect(within(dialog).getByRole("button", { name: "Save" })).toBeDisabled();
  await userEvent.click(within(dialog).getByLabelText("Mon"));
  await userEvent.click(within(dialog).getByLabelText("Run on schedule"));
  await userEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect(await screen.findByText("Backup job nightly saved.")).toBeInTheDocument();
  expect(body).toEqual({ schedule: { at: "03:00", days: ["wed"] }, enabled: false });
});

it("runs a job now and explains a refusal", async () => {
  mockLists({ "backup-job": [nightly] });
  let calls = 0;
  server.use(
    http.post(apiUrl(`/backup-jobs/${nightly.id}/run`), () => {
      calls += 1;
      return calls === 1
        ? HttpResponse.json(nightly)
        : HttpResponse.json(
            { code: "job_running", message: 'backup job "nightly" is already running' },
            { status: 409 },
          );
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Run nightly now" }));
  expect(
    await screen.findByText("nightly starts at the scheduler's next check."),
  ).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "Run nightly now" }));
  expect(
    await screen.findByText("The backup job is already running. Wait for the run to finish."),
  ).toBeInTheDocument();
});

it("deletes a job after confirming and keeps its backups", async () => {
  mockLists({ "backup-job": [nightly] });
  let deleted = false;
  server.use(
    http.delete(apiUrl(`/backup-jobs/${nightly.id}`), () => {
      deleted = true;
      return new HttpResponse(null, { status: 204 });
    }),
  );
  renderPage(<BackupJobsPage />);
  await userEvent.click(await screen.findByRole("button", { name: "Delete backup job nightly" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete backup job nightly?" });
  expect(dialog).toHaveTextContent("The backups it made are kept.");
  await userEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
  expect(await screen.findByText("Backup job nightly deleted.")).toBeInTheDocument();
  expect(deleted).toBe(true);
});

it("lets operators change jobs and viewers only read them", async () => {
  mockLists({ "backup-job": [nightly] });
  const { unmount } = renderPage(<BackupJobsPage />, { as: { ...ADMIN, role: "operator" } });
  expect(await screen.findByRole("button", { name: "Run nightly now" })).toBeEnabled();
  expect(screen.getByRole("button", { name: "Create" })).toBeInTheDocument();
  unmount();
  renderPage(<BackupJobsPage />, { as: { ...ADMIN, role: "viewer" } });
  expect(await screen.findByText("nightly")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Run nightly now" })).not.toBeInTheDocument();
  expect(screen.queryByRole("button", { name: "Create" })).not.toBeInTheDocument();
});
```

`web/src/backups/BackupJobsPage.tsx`를 만든다:

```tsx
import { Pencil, Play, Plus, Trash2 } from "lucide-react";
import { type RefObject, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { api, call } from "@/api/client";
import { useList } from "@/api/live";
import type { BackupJob } from "@/api/types";
import { usePrincipal } from "@/auth/AuthProvider";
import { can } from "@/auth/permissions";
import { backupPools } from "@/backups/backups";
import {
  checkForm,
  emptyForm,
  formatUtc,
  formOf,
  type JobForm,
  KEEP_UNITS,
  keepParts,
  patchOf,
  specOf,
  WEEKDAYS,
} from "@/backups/jobs";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Field } from "@/components/Field";
import { FormDialog } from "@/components/FormDialog";
import { ListPending } from "@/components/ListPending";
import { TableMessage } from "@/components/TableMessage";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { NativeSelect, NativeSelectOption } from "@/components/ui/native-select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage } from "@/i18n/errors";

/** 백업 작업: 목록과 만들기·바꾸기·지우기·지금 실행. 시각은 모두 UTC다(작업 일정이 UTC다). */
export function BackupJobsPage() {
  const { t } = useTranslation();
  const me = usePrincipal();
  const jobs = useList("backup-job");
  const pools = useList("storage-pool");
  // 만들기는 `null`, 바꾸기는 그 작업
  const [editing, setEditing] = useState<BackupJob | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<BackupJob | null>(null);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const opener = useRef<HTMLElement | null>(null);
  const write = can(me.role, "write", "backup-job");
  const list = [...(jobs.data ?? [])].sort((a, b) => a.name.localeCompare(b.name));

  async function run(job: BackupJob) {
    try {
      await call(api.POST("/backup-jobs/{key}/run", { params: { path: { key: job.id } } }));
      toast.success(t("backupJobs.runRequested", { name: job.name }));
    } catch (err) {
      toast.error(errorMessage(t, err));
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h1 id="backup-jobs-title" className="text-2xl font-semibold">
          {t("nav.backupJobs")}
        </h1>
        {write && (
          <div className="flex flex-wrap items-center gap-2 text-sm">
            {/* 풀 목록을 읽어야 백업 풀을 고른다. */}
            {!pools.isSuccess && <ListPending error={pools.error} />}
            <Button
              disabled={!pools.isSuccess || !jobs.isSuccess}
              onClick={(e) => {
                opener.current = e.currentTarget;
                setEditing(null);
              }}
            >
              <Plus />
              {t("common.create")}
            </Button>
          </div>
        )}
      </div>
      <p className="text-sm text-muted-foreground">{t("backupJobs.pageHint")}</p>
      <Table aria-labelledby="backup-jobs-title">
        <TableHeader>
          <TableRow>
            <TableHead>{t("common.name")}</TableHead>
            <TableHead>{t("backupJobs.vms")}</TableHead>
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("backupJobs.schedule")}</TableHead>
            <TableHead>{t("backupJobs.keep")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
            <TableHead>{t("backupJobs.nextRun")}</TableHead>
            <TableHead>{t("backupJobs.lastRun")}</TableHead>
            {write && (
              <TableHead>
                <span className="sr-only">{t("vms.actions")}</span>
              </TableHead>
            )}
          </TableRow>
        </TableHeader>
        <TableBody>
          {list.length === 0 && (
            <TableMessage colSpan={write ? 9 : 8} query={jobs} empty={t("backupJobs.empty")} />
          )}
          {list.map((job) => {
            const { vms, schedule, enabled } = job.spec;
            const { run: running, last_run: last, next_run: next, errors } = job.status;
            const days = schedule.days ?? [];
            const keep = keepParts(job.spec.keep);
            return (
              <TableRow key={job.id}>
                <TableCell className="font-medium">{job.name}</TableCell>
                <TableCell>
                  {"all" in vms ? (
                    t("backupJobs.targets.all")
                  ) : "names" in vms ? (
                    vms.names.join(", ")
                  ) : (
                    <span className="font-mono text-xs">
                      {Object.entries(vms.labels)
                        .map(([k, v]) => `${k}=${v}`)
                        .join(", ")}
                    </span>
                  )}
                </TableCell>
                <TableCell>{job.spec.pool}</TableCell>
                <TableCell>
                  {days.length === 0
                    ? t("backupJobs.everyDay")
                    : days.map((d) => t(`backupJobs.day.${d}`)).join(", ")}{" "}
                  {schedule.at}
                </TableCell>
                <TableCell>
                  {keep.length === 0
                    ? t("backupJobs.keepAll")
                    : keep.map(([unit, n]) => t(`backupJobs.keepUnit.${unit}`, { n })).join(", ")}
                </TableCell>
                <TableCell>
                  {running ? (
                    <>
                      <Badge>{t("backupJobs.state.running")}</Badge>
                      <div className="text-xs text-muted-foreground">
                        {t("backupJobs.waiting", { count: running.queue?.length ?? 0 })}
                      </div>
                    </>
                  ) : (
                    <Badge variant={enabled ? "secondary" : "outline"}>
                      {t(enabled ? "backupJobs.state.enabled" : "backupJobs.state.disabled")}
                    </Badge>
                  )}
                </TableCell>
                <TableCell>{enabled && next !== null ? formatUtc(next) : "—"}</TableCell>
                <TableCell>
                  {last ? (
                    <>
                      {formatUtc(last.finished_at)}
                      <div className="text-xs text-muted-foreground">
                        {t("backupJobs.result", { ok: last.ok, failed: last.failed })}
                      </div>
                    </>
                  ) : (
                    "—"
                  )}
                  {/* 마지막(또는 진행 중인) 실행에서 백업하지 못한 VM */}
                  {errors.length > 0 && (
                    <ul className="max-w-xs text-xs whitespace-normal break-words text-destructive">
                      {errors.map((e) => (
                        <li key={e.vm}>
                          {e.vm}: {e.error}
                        </li>
                      ))}
                    </ul>
                  )}
                </TableCell>
                {write && (
                  <TableCell className="text-right whitespace-nowrap">
                    <Button
                      variant="outline"
                      size="sm"
                      aria-label={t("backupJobs.runNamed", { name: job.name })}
                      // 서버는 진행 중이거나 꺼 둔 작업을 거절한다.
                      disabled={running !== null || !enabled}
                      onClick={() => void run(job)}
                    >
                      <Play />
                      {t("backupJobs.runNow")}
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="ml-2"
                      aria-label={t("backupJobs.editNamed", { name: job.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setEditing(job);
                      }}
                    >
                      <Pencil />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("backupJobs.deleteNamed", { name: job.name })}
                      onClick={(e) => {
                        opener.current = e.currentTarget;
                        setDeleting(job);
                        setDeleteOpen(true);
                      }}
                    >
                      <Trash2 />
                    </Button>
                  </TableCell>
                )}
              </TableRow>
            );
          })}
        </TableBody>
      </Table>
      {editing !== undefined && (
        <JobDialog
          job={editing}
          taken={(jobs.data ?? []).map((j) => j.name)}
          returnFocus={opener}
          onClose={() => setEditing(undefined)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        returnFocus={opener}
        title={t("backupJobs.deleteTitle", { name: deleting?.name })}
        description={t("backupJobs.deleteDescription")}
        confirmLabel={t("common.delete")}
        onConfirm={async () => {
          const job = deleting!;
          await call(api.DELETE("/backup-jobs/{key}", { params: { path: { key: job.id } } }));
          toast.success(t("backupJobs.deleted", { name: job.name }));
        }}
      />
    </div>
  );
}

/** 작업 만들기(`job`이 null)와 바꾸기. 열 때만 그린다. */
function JobDialog({
  job,
  taken,
  returnFocus,
  onClose,
}: {
  job: BackupJob | null;
  /** 있는 작업 이름 */
  taken: string[];
  returnFocus: RefObject<HTMLElement | null>;
  onClose(): void;
}) {
  const { t } = useTranslation();
  const pools = backupPools(useList("storage-pool").data ?? []).map((p) => p.name);
  // 작업의 풀이 이제 백업 풀이 아니어도 고른 값으로 보인다(서버가 저장할 때 거절한다).
  if (job && !pools.includes(job.spec.pool)) {
    pools.unshift(job.spec.pool);
  }
  const [form, setForm] = useState<JobForm>(() => (job ? formOf(job) : emptyForm(pools[0] ?? "")));
  const [errors, setErrors] = useState<Record<string, string>>({});
  const change = (part: Partial<JobForm>) => setForm((f) => ({ ...f, ...part }));
  const unchanged = job !== null && Object.keys(patchOf(job, specOf(form))).length === 0;

  async function submit() {
    const found = checkForm(form, job ? null : taken);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    if (job) {
      await call(
        api.PATCH("/backup-jobs/{key}", {
          params: { path: { key: job.id } },
          body: patchOf(job, specOf(form)),
        }),
      );
      toast.success(t("backupJobs.saved", { name: job.name }));
    } else {
      await call(api.POST("/backup-jobs", { body: { name: form.name, spec: specOf(form) } }));
      toast.success(t("backupJobs.created", { name: form.name }));
    }
  }

  return (
    <FormDialog
      title={job ? t("backupJobs.editTitle", { name: job.name }) : t("backupJobs.createTitle")}
      description={t(job ? "backupJobs.editDescription" : "backupJobs.createDescription")}
      submitLabel={job ? t("common.save") : t("common.create")}
      submitDisabled={pools.length === 0 || unchanged}
      returnFocus={returnFocus}
      onClose={onClose}
      onSubmit={submit}
    >
      {!job && (
        <Field id="job-name" label={t("common.name")} error={errors.name}>
          {(a11y) => (
            <Input
              id="job-name"
              value={form.name}
              onChange={(e) => change({ name: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {pools.length === 0 ? (
        <p className="text-sm text-muted-foreground">{t("backups.noPools")}</p>
      ) : (
        <Field id="job-pool" label={t("backups.pool")} error={errors.pool}>
          {(a11y) => (
            <NativeSelect
              id="job-pool"
              className="w-full"
              value={form.pool}
              onChange={(e) => change({ pool: e.target.value })}
              {...a11y}
            >
              {pools.map((p) => (
                <NativeSelectOption key={p} value={p}>
                  {p}
                </NativeSelectOption>
              ))}
            </NativeSelect>
          )}
        </Field>
      )}
      <Field id="job-target" label={t("backupJobs.target")}>
        <NativeSelect
          id="job-target"
          className="w-full"
          value={form.target}
          onChange={(e) => change({ target: e.target.value as JobForm["target"] })}
        >
          {(["all", "names", "labels"] as const).map((target) => (
            <NativeSelectOption key={target} value={target}>
              {t(`backupJobs.targets.${target}`)}
            </NativeSelectOption>
          ))}
        </NativeSelect>
      </Field>
      {form.target === "names" && (
        <Field
          id="job-names"
          label={t("backupJobs.names")}
          error={errors.names}
          hint={t("backupJobs.namesHint")}
        >
          {(a11y) => (
            <Input
              id="job-names"
              placeholder="web, db"
              value={form.names}
              onChange={(e) => change({ names: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      {form.target === "labels" && (
        <Field
          id="job-labels"
          label={t("backupJobs.labels")}
          error={errors.labels}
          hint={t("backupJobs.labelsHint")}
        >
          {(a11y) => (
            <Input
              id="job-labels"
              placeholder="tier=prod"
              value={form.labels}
              onChange={(e) => change({ labels: e.target.value })}
              {...a11y}
            />
          )}
        </Field>
      )}
      <Field id="job-at" label={t("backupJobs.at")} error={errors.at} hint={t("backupJobs.atHint")}>
        {(a11y) => (
          <Input
            id="job-at"
            placeholder="03:00"
            value={form.at}
            onChange={(e) => change({ at: e.target.value })}
            {...a11y}
          />
        )}
      </Field>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("backupJobs.days")}</legend>
        <div className="flex flex-wrap gap-3">
          {WEEKDAYS.map((day) => (
            <div key={day} className="flex items-center gap-1.5">
              <Checkbox
                id={`job-day-${day}`}
                checked={form.days.includes(day)}
                onCheckedChange={(on) =>
                  change({
                    days: on === true ? [...form.days, day] : form.days.filter((d) => d !== day),
                  })
                }
              />
              <Label htmlFor={`job-day-${day}`}>{t(`backupJobs.day.${day}`)}</Label>
            </div>
          ))}
        </div>
        <p className="text-xs text-muted-foreground">{t("backupJobs.daysHint")}</p>
      </fieldset>
      <fieldset className="flex flex-col gap-1.5">
        <legend className="mb-1.5 text-sm font-medium">{t("backupJobs.retention")}</legend>
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {KEEP_UNITS.map((unit) => (
            <div key={unit} className="flex flex-col gap-1.5">
              <Label htmlFor={`job-keep-${unit}`}>{t(`backupJobs.keepLabel.${unit}`)}</Label>
              <Input
                id={`job-keep-${unit}`}
                inputMode="numeric"
                placeholder="0"
                value={form.keep[unit]}
                aria-invalid={errors.keep ? true : undefined}
                onChange={(e) => change({ keep: { ...form.keep, [unit]: e.target.value } })}
              />
            </div>
          ))}
        </div>
        {errors.keep ? (
          <p className="text-xs text-destructive">{t(errors.keep)}</p>
        ) : (
          <p className="text-xs text-muted-foreground">{t("backupJobs.retentionHint")}</p>
        )}
      </fieldset>
      <div className="flex items-center gap-2">
        <Checkbox
          id="job-enabled"
          checked={form.enabled}
          onCheckedChange={(on) => change({ enabled: on === true })}
        />
        <Label htmlFor="job-enabled">{t("backupJobs.enabled")}</Label>
      </div>
    </FormDialog>
  );
}
```

`web/src/backups/BackupTable.tsx`에서

```tsx
 * 백업 표와 복원·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만).
 */
```

을 다음으로 바꾼다:

```tsx
 * 백업 표와 복원·지우기. VM 상세의 백업 탭과 백업 화면이 쓴다. `showVm`이면 VM 열을 보인다(VM을
 * 지웠으면 만든 때의 이름만). 작업 열은 백업을 만든 백업 작업이다(손으로 만든 백업은 비어 있다).
 */
```

`web/src/backups/BackupTable.tsx`에서

```tsx
  const vms = useList("vm");
  const [restoring, setRestoring] = useState<Backup | null>(null);
```

을 다음으로 바꾼다:

```tsx
  const vms = useList("vm");
  const jobs = useList("backup-job");
  const [restoring, setRestoring] = useState<Backup | null>(null);
```

`web/src/backups/BackupTable.tsx`에서

```tsx
  const actions = restore || remove;
  const columns = 5 + (showVm ? 1 : 0) + (actions ? 1 : 0);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));
```

을 다음으로 바꾼다:

```tsx
  const actions = restore || remove;
  const columns = 6 + (showVm ? 1 : 0) + (actions ? 1 : 0);
  const vmById = new Map((vms.data ?? []).map((v) => [v.id, v]));
  const jobName = (id: string | null | undefined) => {
    if (!id) {
      return "—";
    }
    const job = (jobs.data ?? []).find((j) => j.id === id);
    // 작업을 지워도 백업은 작업 id를 그대로 가진다.
    return job ? job.name : jobs.isSuccess ? t("backups.jobDeleted") : "—";
  };
```

`web/src/backups/BackupTable.tsx`에서

```tsx
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
```

을 다음으로 바꾼다:

```tsx
            <TableHead>{t("backups.pool")}</TableHead>
            <TableHead>{t("backups.job")}</TableHead>
            <TableHead>{t("common.state")}</TableHead>
```

`web/src/backups/BackupTable.tsx`에서

```tsx
                <TableCell>{backup.spec.pool}</TableCell>
                <TableCell>
```

을 다음으로 바꾼다:

```tsx
                <TableCell>{backup.spec.pool}</TableCell>
                <TableCell>{jobName(backup.spec.job)}</TableCell>
                <TableCell>
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
import { resetTasks } from "@/tasks/tasks";
import { backup, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

을 다음으로 바꾼다:

```tsx
import { resetTasks } from "@/tasks/tasks";
import { backup, backupJob, pool, task, vm } from "@/test/fixtures";
import { ADMIN, mockLists, renderPage } from "@/test/render";
```

`web/src/backups/BackupsPage.test.tsx`에서

```tsx
  expect(within(orphan).queryByRole("link")).not.toBeInTheDocument();
});
```

을 다음으로 바꾼다:

```tsx
  expect(within(orphan).queryByRole("link")).not.toBeInTheDocument();
});

it("names the job that made each backup", async () => {
  const nightly = backupJob("nightly", 1);
  const byJob = backup("web-2", web, 3, { spec: { job: nightly.id } });
  // 작업을 지워도 백업은 작업 id를 그대로 가진다.
  const byGoneJob = backup("web-3", web, 4, { spec: { job: backupJob("gone", 2).id } });
  mockLists({ vm: [web], backup: [ofWeb, byJob, byGoneJob], "backup-job": [nightly] });
  renderPage(<BackupsPage />);
  const table = await screen.findByRole("table", { name: "Backups" });
  const headers = within(table).getAllByRole("columnheader");
  const job = async (name: string) =>
    within((await within(table).findByText(name)).closest("tr")!).getAllByRole("cell")[
      headers.findIndex((h) => h.textContent === "Job")
    ];
  expect(await job("web-2")).toHaveTextContent("nightly");
  expect(await job("web-3")).toHaveTextContent("Deleted job");
  expect(await job("web-1")).toHaveTextContent("—");
});
```

`web/src/backups/jobs.test.ts`를 만든다:

```ts
import {
  checkForm,
  emptyForm,
  formatUtc,
  formOf,
  keepParts,
  parseLabels,
  patchOf,
  specOf,
} from "@/backups/jobs";
import { backupJob } from "@/test/fixtures";

it("shows times in UTC", () => {
  expect(formatUtc(1_793_761_200)).toBe("2026-11-04 03:00 UTC");
  expect(formatUtc(0)).toBe("1970-01-01 00:00 UTC");
});

it("reads labels as comma-separated KEY=VALUE pairs", () => {
  expect(parseLabels("tier=prod, env = a=b")).toEqual({ tier: "prod", env: "a=b" });
  expect(parseLabels("")).toEqual({});
  expect(parseLabels("tier")).toBeNull();
  expect(parseLabels("=prod")).toBeNull();
});

it("lists only the retention units in use", () => {
  expect(keepParts({ last: 2, daily: 0, weekly: 4, monthly: 0 })).toEqual([
    ["last", 2],
    ["weekly", 4],
  ]);
  expect(keepParts({ last: 0, daily: 0, weekly: 0, monthly: 0 })).toEqual([]);
  expect(keepParts(undefined)).toEqual([]);
});

it("builds the spec from the form", () => {
  const form = {
    ...emptyForm("bk"),
    name: "nightly",
    target: "labels" as const,
    labels: "tier=prod",
    at: "03:30",
    days: ["sun" as const, "mon" as const],
    keep: { last: "2", daily: "", weekly: "4", monthly: "" },
  };
  expect(checkForm(form, [])).toEqual({});
  // 요일은 월요일부터 차례로 보낸다.
  expect(specOf(form)).toEqual({
    vms: { labels: { tier: "prod" } },
    pool: "bk",
    schedule: { at: "03:30", days: ["mon", "sun"] },
    keep: { last: 2, daily: 0, weekly: 4, monthly: 0 },
    enabled: true,
  });
  expect(specOf({ ...form, target: "names", names: "web, db" }).vms).toEqual({
    names: ["web", "db"],
  });
  expect(specOf({ ...form, target: "all" }).vms).toEqual({ all: true });
});

it("checks the form like the server", () => {
  const form = { ...emptyForm("bk"), name: "nightly", at: "03:00" };
  expect(checkForm(form, [])).toEqual({});
  expect(checkForm({ ...form, name: "Bad name" }, []).name).toBe("forms.name");
  expect(checkForm(form, ["nightly"]).name).toBe("backupJobs.errors.exists");
  // 바꾸기에는 이름이 없다.
  expect(checkForm({ ...form, name: "" }, null)).toEqual({});
  expect(checkForm({ ...form, pool: "" }, []).pool).toBe("backups.errors.pool");
  for (const at of ["3:00", "24:00", "03:60", "0300", ""]) {
    expect(checkForm({ ...form, at }, []).at).toBe("backupJobs.errors.at");
  }
  expect(checkForm({ ...form, at: "23:59" }, [])).toEqual({});
  expect(checkForm({ ...form, target: "names", names: " , " }, []).names).toBe(
    "backupJobs.errors.names",
  );
  expect(checkForm({ ...form, target: "labels", labels: "" }, []).labels).toBe(
    "backupJobs.errors.labels",
  );
  expect(checkForm({ ...form, target: "labels", labels: "tier" }, []).labels).toBe(
    "backupJobs.errors.labels",
  );
  for (const n of ["-1", "1.5", "x", "4294967296"]) {
    expect(checkForm({ ...form, keep: { ...form.keep, daily: n } }, []).keep).toBe(
      "backupJobs.errors.keep",
    );
  }
});

it("reads a job back into the form", () => {
  const job = backupJob("nightly", 1, {
    spec: {
      vms: { labels: { tier: "prod", env: "a" } },
      schedule: { at: "04:00", days: ["sat"] },
      keep: { last: 0, daily: 7, weekly: 0, monthly: 0 },
      enabled: false,
    },
  });
  const form = formOf(job);
  expect(form).toMatchObject({
    name: "nightly",
    pool: "bk",
    target: "labels",
    labels: "tier=prod, env=a",
    at: "04:00",
    days: ["sat"],
    keep: { last: "0", daily: "7", weekly: "0", monthly: "0" },
    enabled: false,
  });
  expect(specOf(form)).toEqual(job.spec);
  expect(formOf(backupJob("n", 2, { spec: { vms: { names: ["web", "db"] } } })).names).toBe(
    "web, db",
  );
});

it("sends only what changed", () => {
  const job = backupJob("nightly", 1);
  const form = formOf(job);
  expect(patchOf(job, specOf(form))).toEqual({});
  expect(
    patchOf(job, specOf({ ...form, at: "04:00", keep: { ...form.keep, weekly: "2" } })),
  ).toEqual({
    schedule: { at: "04:00", days: [] },
    keep: { last: 3, daily: 0, weekly: 2, monthly: 0 },
  });
  expect(patchOf(job, specOf({ ...form, enabled: false, target: "names", names: "web" }))).toEqual({
    vms: { names: ["web"] },
    enabled: false,
  });
  // 요일은 집합으로 비교한다(순서만 다르면 바뀐 것이 아니다).
  const days = backupJob("days", 3, { spec: { schedule: { at: "03:00", days: ["wed", "mon"] } } });
  expect(patchOf(days, specOf(formOf(days)))).toEqual({});
  expect(patchOf(days, specOf({ ...formOf(days), days: ["mon"] }))).toEqual({
    schedule: { at: "03:00", days: ["mon"] },
  });
  // 서버가 보존 규칙을 적지 않았어도 모두 0과 같다.
  const bare = backupJob("bare", 2, { spec: { keep: undefined } });
  expect(
    patchOf(
      bare,
      specOf({ ...formOf(bare), keep: { last: "", daily: "", weekly: "", monthly: "" } }),
    ),
  ).toEqual({});
});
```

`web/src/backups/jobs.ts`를 만든다:

```ts
import type { components } from "@/api/schema";
import type { BackupJob, Weekday } from "@/api/types";
import { validName } from "@/vms/form";

type Spec = BackupJob["spec"];
type Keep = components["schemas"]["Keep"];
export type BackupJobPatch = components["schemas"]["BackupJobPatch"];

/** 월요일부터 */
export const WEEKDAYS: readonly Weekday[] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

export const KEEP_UNITS = ["last", "daily", "weekly", "monthly"] as const;
type KeepUnit = (typeof KEEP_UNITS)[number];

/** 서버의 u32 상한 */
const U32_MAX = 4_294_967_295;

/** 유닉스 초를 `2026-11-04 03:00 UTC`처럼. 작업 일정이 UTC라 화면도 UTC로 보인다. */
export function formatUtc(seconds: number): string {
  const iso = new Date(seconds * 1000).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

/** 쉼표로 나눈 `KEY=VALUE`. 하나라도 잘못되면 null. 값에는 `=`가 있어도 된다. */
export function parseLabels(text: string): Record<string, string> | null {
  const labels: Record<string, string> = {};
  for (const part of text.split(",").map((p) => p.trim())) {
    if (part === "") {
      continue;
    }
    const at = part.indexOf("=");
    const key = at < 0 ? "" : part.slice(0, at).trim();
    if (key === "") {
      return null;
    }
    labels[key] = part.slice(at + 1).trim();
  }
  return labels;
}

function parseNames(text: string): string[] {
  return text
    .split(",")
    .map((n) => n.trim())
    .filter((n) => n !== "");
}

/** 0이 아닌 보존 단위와 개수. 비면 지우지 않는 작업이다. */
export function keepParts(keep: Keep | undefined): [KeepUnit, number][] {
  return KEEP_UNITS.filter((unit) => (keep?.[unit] ?? 0) > 0).map((unit) => [unit, keep![unit]]);
}

/** 만들기·바꾸기 폼의 값. 숫자도 입력 그대로 문자열로 둔다. */
export interface JobForm {
  name: string;
  pool: string;
  target: "all" | "names" | "labels";
  /** 쉼표로 나눈 VM 이름 */
  names: string;
  /** 쉼표로 나눈 `KEY=VALUE` */
  labels: string;
  at: string;
  days: Weekday[];
  keep: Record<KeepUnit, string>;
  enabled: boolean;
}

export function emptyForm(pool: string): JobForm {
  return {
    name: "",
    pool,
    target: "all",
    names: "",
    labels: "",
    at: "",
    days: [],
    keep: { last: "", daily: "", weekly: "", monthly: "" },
    enabled: true,
  };
}

export function formOf(job: BackupJob): JobForm {
  const vms = job.spec.vms;
  const keep = job.spec.keep;
  return {
    name: job.name,
    pool: job.spec.pool,
    target: "all" in vms ? "all" : "names" in vms ? "names" : "labels",
    names: "names" in vms ? vms.names.join(", ") : "",
    labels:
      "labels" in vms
        ? Object.entries(vms.labels)
            .map(([k, v]) => `${k}=${v}`)
            .join(", ")
        : "",
    at: job.spec.schedule.at,
    days: job.spec.schedule.days ?? [],
    keep: {
      last: String(keep?.last ?? 0),
      daily: String(keep?.daily ?? 0),
      weekly: String(keep?.weekly ?? 0),
      monthly: String(keep?.monthly ?? 0),
    },
    enabled: job.spec.enabled,
  };
}

/** 서버와 같은 검사. 오류는 필드별 번역 키다. `taken`은 다른 작업의 이름이고 바꾸기면 null(이름 없음). */
export function checkForm(form: JobForm, taken: string[] | null): Record<string, string> {
  const found: Record<string, string> = {};
  if (taken !== null) {
    if (!validName(form.name)) {
      found.name = "forms.name";
    } else if (taken.includes(form.name)) {
      found.name = "backupJobs.errors.exists";
    }
  }
  if (form.pool === "") {
    found.pool = "backups.errors.pool";
  }
  if (form.target === "names" && parseNames(form.names).length === 0) {
    found.names = "backupJobs.errors.names";
  }
  if (form.target === "labels") {
    const labels = parseLabels(form.labels);
    if (labels === null || Object.keys(labels).length === 0) {
      found.labels = "backupJobs.errors.labels";
    }
  }
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(form.at)) {
    found.at = "backupJobs.errors.at";
  }
  if (
    KEEP_UNITS.some((unit) => {
      const n = form.keep[unit].trim();
      return n !== "" && !(/^\d+$/.test(n) && Number(n) <= U32_MAX);
    })
  ) {
    found.keep = "backupJobs.errors.keep";
  }
  return found;
}

/** 검사한 폼의 스펙. 빈 보존 칸은 0이다. */
export function specOf(form: JobForm): Spec {
  const vms: Spec["vms"] =
    form.target === "all"
      ? { all: true }
      : form.target === "names"
        ? { names: parseNames(form.names) }
        : { labels: parseLabels(form.labels) ?? {} };
  const count = (unit: KeepUnit) => Number(form.keep[unit].trim() || "0");
  return {
    vms,
    pool: form.pool,
    schedule: { at: form.at, days: WEEKDAYS.filter((d) => form.days.includes(d)) },
    keep: {
      last: count("last"),
      daily: count("daily"),
      weekly: count("weekly"),
      monthly: count("monthly"),
    },
    enabled: form.enabled,
  };
}

/** 작업에서 바뀐 필드만(`PATCH` 본문). 일정과 보존 규칙은 통째로 보낸다. 요일은 집합으로 비교한다. */
export function patchOf(job: BackupJob, spec: Spec): BackupJobPatch {
  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  const keep = (k: Keep | undefined) => KEEP_UNITS.map((unit) => k?.[unit] ?? 0);
  const patch: BackupJobPatch = {};
  if (!same(job.spec.vms, spec.vms)) {
    patch.vms = spec.vms;
  }
  if (job.spec.pool !== spec.pool) {
    patch.pool = spec.pool;
  }
  const days = (d: Weekday[] | undefined) => WEEKDAYS.filter((w) => d?.includes(w));
  const schedule = { at: job.spec.schedule.at, days: days(job.spec.schedule.days) };
  if (!same(schedule, { at: spec.schedule.at, days: days(spec.schedule.days) })) {
    patch.schedule = spec.schedule;
  }
  if (!same(keep(job.spec.keep), keep(spec.keep))) {
    patch.keep = spec.keep;
  }
  if (job.spec.enabled !== spec.enabled) {
    patch.enabled = spec.enabled;
  }
  return patch;
}
```

`web/src/i18n/en.json`에서

```json
    "backup_busy": "A task on this backup (backing up, restoring or deleting) is in progress. Try again when it has finished.",
    "backup_not_ready": "The backup is not ready. Only finished backups can be restored."
  },
```

을 다음으로 바꾼다:

```json
    "backup_busy": "A task on this backup (backing up, restoring or deleting) is in progress. Try again when it has finished.",
    "backup_not_ready": "The backup is not ready. Only finished backups can be restored.",
    "job_running": "The backup job is already running. Wait for the run to finish.",
    "job_disabled": "The backup job is off. Turn on \"Run on schedule\" first."
  },
```

`web/src/i18n/en.json`에서

```json
    "backups": "Backups",
    "networks": "Networks",
```

을 다음으로 바꾼다:

```json
    "backups": "Backups",
    "backupJobs": "Backup jobs",
    "networks": "Networks",
```

`web/src/i18n/en.json`에서

```json
    "pool": "Backup pool",
    "created": "Created",
```

을 다음으로 바꾼다:

```json
    "pool": "Backup pool",
    "job": "Job",
    "jobDeleted": "Deleted job",
    "created": "Created",
```

`web/src/i18n/en.json`에서

```json
      "pool": "Choose a storage pool."
    }
```

을 다음으로 바꾼다:

```json
      "pool": "Choose a storage pool."
    }
  },
  "backupJobs": {
    "pageHint": "Back up the chosen VMs at a set time and delete old backups by the retention rules. Times are in UTC. The cluster leader runs the jobs, at most a few backups per node at a time.",
    "empty": "No backup jobs.",
    "vms": "VMs",
    "schedule": "Schedule (UTC)",
    "keep": "Keep",
    "nextRun": "Next run",
    "lastRun": "Last run",
    "everyDay": "Every day",
    "day": {
      "mon": "Mon",
      "tue": "Tue",
      "wed": "Wed",
      "thu": "Thu",
      "fri": "Fri",
      "sat": "Sat",
      "sun": "Sun"
    },
    "keepAll": "All backups",
    "keepUnit": {
      "last": "last {{n}}",
      "daily": "{{n}} daily",
      "weekly": "{{n}} weekly",
      "monthly": "{{n}} monthly"
    },
    "state": {
      "running": "Running",
      "enabled": "On",
      "disabled": "Off"
    },
    "waiting_one": "{{count}} VM waiting",
    "waiting_other": "{{count}} VMs waiting",
    "result": "{{ok}} ok, {{failed}} failed",
    "runNow": "Run now",
    "runNamed": "Run {{name}} now",
    "runRequested": "{{name}} starts at the scheduler's next check.",
    "editNamed": "Edit {{name}}",
    "deleteNamed": "Delete backup job {{name}}",
    "createTitle": "Create backup job",
    "createDescription": "Backs up the chosen VMs to a backup pool on the chosen days and deletes the backups the retention rules no longer keep.",
    "editTitle": "Edit {{name}}",
    "editDescription": "Changes apply from the next run. A run in progress finishes.",
    "target": "VMs to back up",
    "targets": {
      "all": "All VMs",
      "names": "VMs by name",
      "labels": "VMs with labels"
    },
    "names": "VM names",
    "namesHint": "Separate names with commas.",
    "labels": "Labels",
    "labelsHint": "KEY=VALUE pairs separated by commas. A VM must have all of them.",
    "at": "Time (UTC)",
    "atHint": "HH:MM in UTC, such as 03:00.",
    "days": "Days",
    "daysHint": "Leave every day unchecked to run every day.",
    "retention": "Retention",
    "retentionHint": "Keeps the newest backups (Last), then the newest backup of each day, ISO week and month, counted per VM. Leave all at 0 to keep every backup. Failed backups are deleted after a later success.",
    "keepLabel": {
      "last": "Last",
      "daily": "Daily",
      "weekly": "Weekly",
      "monthly": "Monthly"
    },
    "enabled": "Run on schedule",
    "created": "Backup job {{name}} created.",
    "saved": "Backup job {{name}} saved.",
    "deleted": "Backup job {{name}} deleted.",
    "deleteTitle": "Delete backup job {{name}}?",
    "deleteDescription": "The job stops running. The backups it made are kept.",
    "errors": {
      "exists": "A backup job with this name already exists.",
      "names": "Enter at least one VM name.",
      "labels": "Enter at least one KEY=VALUE label.",
      "at": "Enter a time from 00:00 to 23:59.",
      "keep": "Enter whole numbers of 0 or more."
    }
```

`web/src/i18n/i18n.test.ts`에서

```ts
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

을 다음으로 바꾼다:

```ts
  );
  expect(errorMessage(t, new ApiError(409, "job_disabled", 'backup job "n" is disabled'))).toBe(
    'The backup job is off. Turn on "Run on schedule" first.',
  );
  expect(errorMessage(t, new ApiError(400, "invalid_request", "memory_mib must be > 0"))).toBe(
```

`web/src/i18n/ko.json`에서

```json
    "backup_busy": "이 백업의 작업(백업, 복원, 삭제)이 진행 중입니다. 끝난 뒤 다시 시도하세요.",
    "backup_not_ready": "백업이 준비되지 않았습니다. 다 만든 백업만 복원할 수 있습니다."
  },
```

을 다음으로 바꾼다:

```json
    "backup_busy": "이 백업의 작업(백업, 복원, 삭제)이 진행 중입니다. 끝난 뒤 다시 시도하세요.",
    "backup_not_ready": "백업이 준비되지 않았습니다. 다 만든 백업만 복원할 수 있습니다.",
    "job_running": "백업 작업이 이미 실행 중입니다. 실행이 끝난 뒤 다시 시도하세요.",
    "job_disabled": "꺼 둔 백업 작업입니다. 먼저 \"일정대로 실행\"을 켜세요."
  },
```

`web/src/i18n/ko.json`에서

```json
    "backups": "백업",
    "networks": "네트워크",
```

을 다음으로 바꾼다:

```json
    "backups": "백업",
    "backupJobs": "백업 작업",
    "networks": "네트워크",
```

`web/src/i18n/ko.json`에서

```json
    "pool": "백업 풀",
    "created": "만든 시각",
```

을 다음으로 바꾼다:

```json
    "pool": "백업 풀",
    "job": "작업",
    "jobDeleted": "삭제한 작업",
    "created": "만든 시각",
```

`web/src/i18n/ko.json`에서

```json
      "pool": "스토리지 풀을 고르세요."
    }
```

을 다음으로 바꾼다:

```json
      "pool": "스토리지 풀을 고르세요."
    }
  },
  "backupJobs": {
    "pageHint": "정한 시각에 고른 VM들을 백업하고 보존 규칙에 맞지 않는 옛 백업을 지웁니다. 시각은 UTC입니다. 클러스터 리더가 작업을 실행하며 노드마다 동시에 도는 백업 수에 제한이 있습니다.",
    "empty": "백업 작업이 없습니다.",
    "vms": "VM",
    "schedule": "일정(UTC)",
    "keep": "보존",
    "nextRun": "다음 실행",
    "lastRun": "마지막 실행",
    "everyDay": "매일",
    "day": {
      "mon": "월",
      "tue": "화",
      "wed": "수",
      "thu": "목",
      "fri": "금",
      "sat": "토",
      "sun": "일"
    },
    "keepAll": "모두 남김",
    "keepUnit": {
      "last": "최근 {{n}}개",
      "daily": "일별 {{n}}",
      "weekly": "주별 {{n}}",
      "monthly": "월별 {{n}}"
    },
    "state": {
      "running": "실행 중",
      "enabled": "켜짐",
      "disabled": "꺼짐"
    },
    "waiting_one": "VM {{count}}개 대기",
    "waiting_other": "VM {{count}}개 대기",
    "result": "성공 {{ok}}, 실패 {{failed}}",
    "runNow": "지금 실행",
    "runNamed": "{{name}} 지금 실행",
    "runRequested": "{{name}}: 스케줄러가 다음 확인 때 실행을 시작합니다.",
    "editNamed": "{{name}} 편집",
    "deleteNamed": "백업 작업 {{name}} 삭제",
    "createTitle": "백업 작업 만들기",
    "createDescription": "고른 요일에 VM들을 백업 풀에 백업하고, 보존 규칙이 더는 남기지 않는 백업을 지웁니다.",
    "editTitle": "{{name}} 편집",
    "editDescription": "바꾼 내용은 다음 실행부터 적용됩니다. 진행 중인 실행은 끝까지 갑니다.",
    "target": "백업할 VM",
    "targets": {
      "all": "모든 VM",
      "names": "이름으로 고르기",
      "labels": "라벨로 고르기"
    },
    "names": "VM 이름",
    "namesHint": "쉼표로 나눠 적습니다.",
    "labels": "라벨",
    "labelsHint": "KEY=VALUE를 쉼표로 나눠 적습니다. VM에 모든 라벨이 있어야 합니다.",
    "at": "시각(UTC)",
    "atHint": "UTC 기준 HH:MM. 예: 03:00",
    "days": "요일",
    "daysHint": "아무 요일도 고르지 않으면 매일 실행합니다.",
    "retention": "보존 규칙",
    "retentionHint": "최근 백업(최근)을 남기고, 그다음 날·ISO 주·달마다 가장 새 백업을 하나씩 남깁니다. VM마다 따로 셉니다. 모두 0이면 지우지 않습니다. 실패한 백업은 뒤에 성공하면 지웁니다.",
    "keepLabel": {
      "last": "최근",
      "daily": "일별",
      "weekly": "주별",
      "monthly": "월별"
    },
    "enabled": "일정대로 실행",
    "created": "백업 작업 {{name}}을(를) 만들었습니다.",
    "saved": "백업 작업 {{name}}을(를) 저장했습니다.",
    "deleted": "백업 작업 {{name}}을(를) 삭제했습니다.",
    "deleteTitle": "백업 작업 {{name}}을(를) 삭제할까요?",
    "deleteDescription": "작업이 더는 실행되지 않습니다. 작업이 만든 백업은 남습니다.",
    "errors": {
      "exists": "같은 이름의 백업 작업이 있습니다.",
      "names": "VM 이름을 하나 이상 적으세요.",
      "labels": "KEY=VALUE 라벨을 하나 이상 적으세요.",
      "at": "00:00부터 23:59 사이의 시각을 적으세요.",
      "keep": "0 이상의 정수를 적으세요."
    }
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

을 다음으로 바꾼다:

```tsx
  expect(screen.queryByRole("link", { name: "Users" })).not.toBeInTheDocument();
  // 보안 그룹과 백업, 백업 작업은 누구나 읽는다.
  expect(screen.getByRole("link", { name: "Security groups" })).toHaveAttribute(
```

`web/src/layout/AppLayout.test.tsx`에서

```tsx
  expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/backups");
});
```

을 다음으로 바꾼다:

```tsx
  expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/backups");
  expect(screen.getByRole("link", { name: "Backup jobs" })).toHaveAttribute("href", "/backup-jobs");
});
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  Boxes,
  Cpu,
```

을 다음으로 바꾼다:

```tsx
  Boxes,
  CalendarClock,
  Cpu,
```

`web/src/layout/AppLayout.tsx`에서

```tsx
  { to: "/backups", key: "nav.backups", icon: Archive, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
```

을 다음으로 바꾼다:

```tsx
  { to: "/backups", key: "nav.backups", icon: Archive, end: false },
  { to: "/backup-jobs", key: "nav.backupJobs", icon: CalendarClock, end: false },
  { to: "/networks", key: "nav.networks", icon: Network, end: false },
```

`web/src/test/fixtures.ts`에서

```ts
  Backup,
  Node,
```

을 다음으로 바꾼다:

```ts
  Backup,
  BackupJob,
  Node,
```

`web/src/test/fixtures.ts`에서

```ts
    },
  };
}
```

을 다음으로 바꾼다:

```ts
    },
  };
}

/** 테스트용 백업 작업: 매일 03:00(UTC)에 모든 VM을 `bk`에 백업하고 최신 3개를 남긴다. */
export function backupJob(
  name: string,
  n: number,
  extra: { spec?: Partial<BackupJob["spec"]>; status?: Partial<BackupJob["status"]> } = {},
): BackupJob {
  return {
    id: `019a0f3c-0000-7000-8007-${String(n).padStart(12, "0")}`,
    name,
    revision: 1,
    labels: {},
    spec: {
      vms: { all: true },
      pool: "bk",
      schedule: { at: "03:00", days: [] },
      keep: { last: 3, daily: 0, weekly: 0, monthly: 0 },
      enabled: true,
      ...extra.spec,
    },
    status: { next_run: 1_793_761_200, run: null, last_run: null, errors: [], ...extra.status },
  };
}
```

`web/src/test/render.tsx`에서

```tsx
  backup: "/backups",
};
```

을 다음으로 바꾼다:

```tsx
  backup: "/backups",
  "backup-job": "/backup-jobs",
};
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1011개, 실패 0

Run: `cargo test -p oplctl`
Expected: PASS

Run: `pnpm -C web format && pnpm -C web typecheck && pnpm -C web lint && pnpm -C web test`
Expected: PASS 382개

- [ ] **Step 3: 커밋**

```bash
git add crates web
git commit -m "feat: oplctl·웹에서 백업 작업 — backup-job 명령(만들기·목록·읽기·바꾸기·지우기·지금 실행), 백업 작업 화면(일정·보존·다음과 마지막 실행 UTC, 오류), 사이드바, 백업 목록의 작업 열, watch 종류 backup-job"
```

---

### Task 5: 실제 시험과 문서

`crates/opld/tests/backup.rs`에 `backup_job_keeps_last_backups_of_labeled_vms`(기능 `qemu-it`): `[backup] tick_secs = 1`, 라벨 `tier=prod`인 VM만 고르는 `--keep-last 2` 작업을 세 번 실행하면 그 VM의 백업이 2개만 남고(가장 오래된 것과 그 디렉터리가 지워짐) 다른 VM은 백업이 없으며, 작업을 지워도 백업은 남는다. CI `check`의 storage-it 단계에 `backup_tasks`·`backup_job_runs`. README 백업 작업 절, 설계 "구현(계획 28)" 메모와 알려진 틈, 처음 분해 문서(하위 프로젝트 7 완료).

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`
- Modify: `crates/opld/tests/backup.rs`
- Modify: `docs/superpowers/specs/2026-10-20-opl-node-core-design.md`
- Modify: `docs/superpowers/specs/2026-11-04-backup-design.md`

- [ ] **Step 1: 코드 (테스트 포함)**

아래 편집을 위에서부터 차례로 적용한다. 각 '바꾼다' 블록의 앞부분(찾을 코드)은 그 파일에서 한 번만 나온다.

`.github/workflows/ci.yml`에서

```yaml
          cargo test -p opl-api --features storage-it --test volumes
```

을 다음으로 바꾼다:

```yaml
          cargo test -p opl-api --features storage-it --test volumes
          cargo test -p opl-api --features storage-it --test backup_tasks --test backup_job_runs
```

`README.md`에서

```markdown
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [pools]
```

을 다음으로 바꾼다:

```markdown
# task_max_finished = 1000  # 끝난 작업을 이보다 많이 남기지 않는다(오래된 것부터 지운다)
# [backup]
# tick_secs = 60          # 백업 작업을 확인하는 간격(초, 리더 노드)
# max_concurrent = 2      # 노드마다 동시에 도는 백업 작업(Task)의 최대 수
# [pools]
```

`README.md`에서

```markdown

- **한계.** 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존
  VM 덮어쓰기는 없다. 예약 백업과 보존 규칙은 계획 28에서 온다(지금은 손으로 만들고 지운다).
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
```

을 다음으로 바꾼다:

````markdown

**백업 작업(예약 백업과 보존).** 백업 작업은 정한 VM들을 정한 시각에 백업 풀로 백업하고, 보존 규칙에 맞지 않는
오래된 백업을 지운다.

```bash
oplctl backup-job create nightly --pool bk --all --at 03:00 --keep-daily 7 --keep-weekly 4   # 모든 VM, 매일 03:00 UTC
oplctl backup-job create prod --pool bk --label tier=prod --at 01:30 --days mon,thu --keep-last 3
oplctl backup-job create db --pool bk --vm db --vm db2 --at 02:00 --keep-last 2 --keep-monthly 6 --disabled
oplctl backup-job list
# NAME     POOL  VMS        SCHEDULE              KEEP              ENABLED  NEXT                  LAST
# nightly  bk    all        daily 03:00 UTC       daily=7,weekly=4  true     2026-11-05 03:00 UTC  2026-11-04 03:00 UTC (12 ok, 1 failed)
oplctl backup-job get nightly                  # 일정·보존·다음 실행·마지막 실행 결과와 VM별 오류(JSON)
oplctl backup-job set prod --at 04:00 --keep-last 5 --enabled true   # 준 값만 바꾼다(보존은 준 단위만, 나머지는 그대로)
oplctl backup-job run nightly                  # 지금 실행(스케줄러의 다음 확인 때 시작한다)
oplctl backup-job delete nightly               # 작업만 지운다. 만든 백업은 남는다
```

- **대상.** `--all`(모든 VM), `--vm NAME`(여러 번), `--label KEY=VALUE`(여러 번이면 모두 맞는 VM) 중 하나다. VM은
  실행할 때마다 다시 고르므로 나중에 만든 VM과 라벨을 붙인 VM도 들어간다.
- **일정은 UTC다.** `--at HH:MM`(UTC)에, `--days`(`mon`…`sun`)를 주면 그 요일에만 돈다(없으면 매일). 리더 노드가
  1분마다 확인하므로 정한 시각에서 1분쯤 늦게 시작할 수 있다. 리더가 꺼져 있던 사이 놓친 실행은 여러 번이어도 한
  번만 하고, 리더가 바뀌면 새 리더가 진행 중인 실행을 이어 간다. 작업을 끄면(`--disabled`, `set --enabled false`)
  예약 실행도 지금 실행도 하지 않는다(진행 중인 실행은 이미 요청한 백업만 끝까지 기다린다).
- **한 번에 하나씩.** 실행은 대상 VM을 이름 순으로 하나씩 백업 작업(Task)으로 요청한다. 한 노드에서 동시에 도는
  백업은 opld 설정 `[backup] max_concurrent`(기본 2, 손으로 만든 백업도 센다)까지이고 나머지는 차례를 기다린다.
  이미 백업 중인 VM, 실행 중이라 백업할 수 없는 VM(위의 409와 같은 이유), `--vm`으로 골랐는데 없는 VM은 건너뛰고
  작업의 `errors`에 남긴다. 실행 도중 지운 VM은 그냥 빠진다. 작업이 만든 백업의 이름은 보통처럼
  `{VM 이름}-{UTC 시각}`(겹치면 `-2`…, 63자를 넘지 않게 VM 이름 부분을 줄인다)이고, 백업의
  `spec.job`(`oplctl backup get`)과 웹 백업 목록의 작업 열에 작업이 보인다.
- **마지막 실행.** 큐가 비고 이번에 만든 백업이 모두 끝나면 `last_run`에 시작·끝 시각과 성공(`ok`)·실패(`failed`,
  실패한 백업과 백업하지 못한 VM) 수를 적는다. 백업하던 노드가 죽어 백업 작업(Task)이 끝나지 못한 백업도 실패로
  센다. 실행 중에 `backup-job run`은 409 `job_running`이다. 일정을 바꿔도 이미 요청한 지금 실행은 그대로 돈다.
- **보존 규칙(Proxmox와 같다).** 실행이 끝나면 VM마다 그 작업이 만든 `ready` 백업을 최신부터 보며 `--keep-last N`개를
  먼저 남기고, 그다음 `--keep-daily`(UTC 날짜)·`--keep-weekly`(ISO 주)·`--keep-monthly`(달)마다 그 기간의 가장 새
  백업을 하나씩 N개 기간까지 남긴다(어느 것이 더 새 것인지는 만든 순서로 정하고, 기간은 만든 시각으로 나눈다).
  남지 않은 백업은 지운다. 예: `--keep-last 2 --keep-daily 7`이면 가장 새 백업 둘과, 지난 7일(백업이 있는 날
  기준) 각각의 마지막 백업이 남는다. 보존 값을 모두 0으로 두면(기본) 지우지 않는다.
  그 작업의 실패한 백업은 같은 VM의 더 새 성공한 백업이 생기면 지운다. 손으로 만든 백업과 다른 작업의 백업은
  건드리지 않는다.
- **작업을 지워도 백업은 남는다.** 그 백업은 더는 보존 규칙으로 지워지지 않으니 `oplctl backup delete`로 지운다.
- 웹은 사이드바 "백업" 아래 "백업 작업" 화면(만들기·편집·지우기·지금 실행, 다음·마지막 실행 시각은 UTC로 표시)에서
  한다. API는 `GET`·`POST /api/v1/backup-jobs`, `GET`·`PATCH`·`DELETE /api/v1/backup-jobs/{name}`,
  `POST /api/v1/backup-jobs/{name}/run`이고, 읽기는 모든 사용자, 쓰기는 operator 이상이다. 스케줄러의 확인 간격은
  `[backup] tick_secs`(기본 60초)다.

- **한계.** 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존
  VM 덮어쓰기는 없다.
- **남을 수 있는 것.** 작업을 만들기 전에 실패하거나 작업이 사라져 `pending`에 머문 백업은 저절로 치워지지 않는다 —
````

`README.md`에서

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기), 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

을 다음으로 바꾼다:

```markdown
웹 UI로 CLI의 관리 기능을 모두 할 수 있다: 대시보드, 가상 머신(목록·상세·만들기·편집·동작·콘솔·스냅샷·복제·예약 스냅샷·마이그레이션·백업),
볼륨(빈 볼륨, 이미지 업로드, 크기 늘리기), 스토리지 풀, 백업(목록·복원·지우기)과 백업 작업, 네트워크, 보안 그룹, 노드(유지보수 모드, 죽은 노드 제거), 작업,
사용자(admin), API 토큰, 클러스터(멤버, CA, 조인 토큰), 내 비밀번호 변경. 노드를 더할 때는 새 노드에서
```

`crates/opld/tests/backup.rs`에서

```rust
    }
    d.stop();
}
```

을 다음으로 바꾼다:

```rust
    }
    d.stop();
}

/// 백업 작업 `job`의 실행 하나가 끝날 때까지 기다리고 작업을 돌려준다. `before`는 실행을 요청하기 전의
/// `last_run`이다.
async fn wait_job_run(c: &Client, job: &str, before: &Value) -> Value {
    let deadline = std::time::Instant::now() + common::WAIT;
    loop {
        let job = c.get(&format!("/backup-jobs/{job}")).await.unwrap();
        if job["status"]["run"].is_null() && &job["status"]["last_run"] != before {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for a run: {job}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// VM `vm`의 백업 id(만든 순서)
async fn backup_ids(d: &Daemon, vm: &str) -> Vec<String> {
    let list = d.ctl_json(&["backup", "list", "--vm", vm]).await;
    let mut backups = list.as_array().unwrap().clone();
    backups.sort_by_key(|b| b["status"]["created_at"].as_u64());
    backups
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn backup_job_keeps_last_backups_of_labeled_vms() {
    let mut d = Daemon::new("node1", "[backup]\ntick_secs = 1\n");
    setup(&mut d).await;
    let c = client(&d);
    for (vm, volume) in [("web", "web-root"), ("db", "db-root")] {
        create_volume(&d, volume, "qcow2").await;
        d.ctl(&["vm", "create", vm, "--memory", "256", "--disk", volume])
            .await
            .unwrap();
    }
    // oplctl에는 라벨을 넣는 길이 없어 API로 넣는다.
    let web = c.get("/vms/web").await.unwrap();
    c.put(
        "/vms/web",
        &json!({ "spec": web["spec"], "labels": { "tier": "prod" } }),
    )
    .await
    .unwrap();

    let out = d
        .ctl(&[
            "backup-job",
            "create",
            "nightly",
            "--pool",
            "bk",
            "--label",
            "tier=prod",
            "--at",
            "00:00",
            "--keep-last",
            "2",
        ])
        .await
        .unwrap();
    assert!(
        out.starts_with("backup job nightly created; next run ") && out.ends_with(" 00:00 UTC\n"),
        "{out}"
    );
    let job = c.get("/backup-jobs/nightly").await.unwrap();

    // 지금 실행을 세 번. 매번 실행이 끝나기를 기다린다.
    let mut first = Vec::new();
    for i in 0..3 {
        let before = c.get("/backup-jobs/nightly").await.unwrap()["status"]["last_run"].clone();
        let out = d.ctl(&["backup-job", "run", "nightly"]).await.unwrap();
        assert_eq!(
            out,
            "backup job nightly will run at the scheduler's next check\n"
        );
        let done = wait_job_run(&c, "nightly", &before).await;
        let last_run = &done["status"]["last_run"];
        assert_eq!(last_run["ok"], 1, "{done}");
        assert_eq!(last_run["failed"], 0, "{done}");
        assert_eq!(done["status"]["errors"], json!([]), "{done}");
        if i == 0 {
            first = backup_ids(&d, "web").await;
            assert_eq!(first.len(), 1);
        }
    }

    // 보존 규칙(마지막 2개)으로 가장 오래된 백업이 지워진다. 라벨이 없는 VM은 백업하지 않는다.
    common::eventually("oldest backup pruned", || async {
        backup_ids(&d, "web").await.len() == 2
    })
    .await;
    let kept = backup_ids(&d, "web").await;
    assert!(!kept.contains(&first[0]), "{kept:?} {first:?}");
    assert!(
        !d.path("bk").join("backups").join(&first[0]).exists(),
        "pruned backup directory left"
    );
    assert_eq!(backup_ids(&d, "db").await, Vec::<String>::new());
    for id in &kept {
        let b = c.get(&format!("/backups/{id}")).await.unwrap();
        assert_eq!(b["spec"]["job"], job["id"], "{b}");
        assert_eq!(b["status"]["phase"], "ready", "{b}");
    }

    // 작업을 지워도 백업은 남는다.
    let out = d.ctl(&["backup-job", "delete", "nightly"]).await.unwrap();
    assert_eq!(out, "backup job nightly deleted; its backups are kept\n");
    assert_eq!(d.ctl_json(&["backup-job", "list"]).await, json!([]));
    assert_eq!(backup_ids(&d, "web").await, kept);

    for id in &kept {
        d.ctl(&["backup", "delete", id]).await.unwrap();
    }
    for vm in ["web", "db"] {
        d.ctl(&["vm", "delete", vm]).await.unwrap();
    }
    for volume in ["web-root", "db-root"] {
        d.ctl(&["volume", "delete", volume]).await.unwrap();
    }
    d.stop();
}
```

`docs/superpowers/specs/2026-10-20-opl-node-core-design.md`에서

```markdown

구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`, 리더의 스케줄러, 노드당 동시 실행 제한, 보존 규칙, 남음)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 진행 중이다.
```

을 다음으로 바꾼다:

```markdown

구현: 하위 프로젝트 7은 계획 27(백업과 복원: dir·NFS 풀의 `backups` 표시, `Backup` 리소스, 실행 중·꺼진 VM의 crash-consistent 전체 백업을 디스크마다 압축 qcow2와 `manifest.json`으로, 새 VM으로 복원, 지우기, API·oplctl·웹, 실제 시험, 완료)과 계획 28(백업 작업과 보존: `BackupJob`(대상 VM 고르기·UTC 일정·보존 규칙), 리더의 `BackupJobScheduler`, 노드당 동시 백업 제한, Proxmox식 보존 규칙으로 지우기, API·oplctl·웹, 실제 시험, 완료)으로 나눴다(`2026-11-04-backup-design.md`). 하위 프로젝트 7은 끝났다. 증분·변경 블록 추적, 중복 제거, 암호화, 원격(S3) 저장소, 게스트 에이전트로 얼리기, 파일 단위 복원, 기존 VM 덮어쓰기는 넣지 않았다.
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown

### 3.2 스케줄러 (리더)
```

을 다음으로 바꾼다:

```markdown

구현(계획 28): `opl_types::backup_job`의 `BackupJob = Resource<BackupJobSpec { vms: VmSelector, pool, schedule: JobSchedule { at: "HH:MM"(UTC), days: Vec<Weekday>(mon..sun, 비면 매일, 중복 거절) }, keep: Keep { last, daily, weekly, monthly }(모두 기본 0, 모두 0이면 지우지 않음), enabled(기본 true) }, BackupJobStatus { next_run: Option<u64>, run: Option<JobRun { started_at, queue: Vec<VM id>(아직 꺼내지 않은 VM), backups: Vec<백업 id>(이번 실행에서 만든 백업) }>, last_run: Option<JobRunResult { started_at, finished_at, ok, failed }>, errors: Vec<JobError { vm, error }> }>`, `Kind::BackupJob`(`backup-job`, watch 종류에도 있다). `VmSelector`는 안에서는 enum `All|Names|Labels`이고 JSON은 `{"all": true}`·`{"names": [...]}`·`{"labels": {...}}` 중 정확히 하나다(OpenAPI는 손으로 쓴 `oneOf`; 빈 `names`·`labels`는 검증 오류). 진행 중 실행을 `status.run`에 두어 리더가 바뀌어도 이어 간다(설계의 "`last_run`을 보고 같은 날 두 번 돌지 않게" 대신 실행을 시작할 때 `next_run`을 바로 다음 시각으로 옮긴다). 순수 함수 `next_run(schedule, after)`(after보다 뒤의 첫 시각, 분 단위, UTC), `VmSelector::matches(&Vm)`(라벨은 모두 맞아야), `prune(&[(id, created_at)], &Keep) -> 지울 id`(아래; 최신 순서는 id(UUIDv7)로, 기간은 `created_at`으로). API는 opl-api `backup_jobs.rs`: `GET`·`POST /backup-jobs`(201, 만들 때 `next_run`을 적는다), `GET`·`PATCH`·`DELETE /backup-jobs/{key}`(`BackupJobPatch { vms?, pool?, schedule?, keep?(통째 교체), enabled? }`, 빈 PATCH는 400, 일정을 바꾸거나 다시 켜면 `next_run`을 지금부터 다시 계산 — 켜 둔 작업의 `next_run`이 이미 지났으면(지금 실행 요청이 기다림) 그대로 두고, 새 `next_run`을 스펙보다 먼저 써서 스펙 쓰기가 실패해도 지난 실행을 바로 돌리지 않는다; 지우기는 204이고 백업은 `job`을 그대로 둔 채 남는다), `POST /backup-jobs/{key}/run`(200, `next_run = now`; 진행 중이면 409 `job_running`, 꺼 둔 작업이면 409 `job_disabled`). 만들기·바꾸기는 검증(400, 영어 오류)과 풀이 있고 `backups`인지(400)를 보고 `schedule.days`를 주의 순서로 정리해 저장한다. 권한은 백업과 같다: 쓰기는 operator 이상, 읽기는 모두. 백업 만들기 검사와 Task 만들기(`check_backup`·`start_backup`)와 지우기 Task 만들기(`start_backup_delete`)는 `backups.rs`에서 함수로 빼 API와 스케줄러가 함께 쓴다(권한 검사는 API에만).

### 3.2 스케줄러 (리더)
```

`docs/superpowers/specs/2026-11-04-backup-design.md`에서

```markdown
- oplctl `backup-job create|list|get|set|delete|run-now`, 웹 백업 작업 페이지.
```

을 다음으로 바꾼다:

```markdown
- oplctl `backup-job create|list|get|set|delete|run-now`, 웹 백업 작업 페이지.

구현(계획 28): opl-api `BackupJobScheduler::spawn(state, BackupJobSchedulerConfig { tick, max_concurrent })`(모든 노드에서 띄우고 리더에서만 일한다, `shutdown()`), opld `[backup] tick_secs`(기본 60)·`max_concurrent`(기본 2, 1 이상). tick마다 작업을 이름 순으로 본다. (1) 실행이 없고 켜져 있고 `next_run <= now`면 실행을 시작한다: 이름 순으로 `matches`인 VM의 id를 `queue`에, `errors`에는 `names`에 있는데 VM이 없는 이름마다 "no VM named \"…\""를 넣고, `next_run = next_run(schedule, now)`(놓친 실행이 여럿이어도 한 번) — revision을 검사해 쓴다. (2) 실행 중이면 queue의 VM마다: 없어졌으면 빼고, 이번 실행이 이 VM의 백업을 이미 만들었으면(`spec.job`이 작업이고 백업 id(UUIDv7)의 초가 `run.started_at` 이상이며 `last_run.finished_at`보다 뒤 — 상태에 적지 못한 채 쓰기가 실패했거나 리더가 바뀌었다) 그 백업을 `run.backups`에 받아들이고, 끝나지 않은 `VmBackup` Task가 이미 있으면 빼고 오류 "a backup of this VM is already running", 백업 만들기 검사가 거절하면(실행 중이라 스냅샷 불가, 노드 없음 등 4xx) 빼고 그 메시지를 오류로, 고른 노드의 끝나지 않은 `VmBackup` Task(손으로 만든 것도)가 `max_concurrent` 이상이면 queue에 남겨 다음 tick에(다른 노드의 VM은 계속 꺼낸다), 아니면 백업을 요청해 `run.backups`에 적는다. 이름은 `default_backup_name`이고 겹치면 `-2`, `-3`…(VM 이름 부분을 번호 길이만큼 줄여 63자를 넘지 않는다; `start_backup`도 이름을 검사한다), `spec.job = 작업 id`. 작업이 꺼졌으면 VM을 더 꺼내지 않고 queue를 비운다(이미 만든 백업은 기다린다). (3) queue가 비고 이번 실행의 백업이 모두 `ready`·`failed`이거나 없어졌거나 채우는 끝나지 않은 `VmBackup` Task가 없는 `pending`이면(죽은 노드의 Task를 janitor가 실패 처리했다 — 실패로 세고 오류 "backup task ended without finishing the backup") `last_run = { started_at, finished_at: now, ok: ready 수, failed: failed 수 + errors 수 }`, 실패한 백업의 오류를 `errors`에 더하고 `run = None`. 그다음 보존: 이 작업의 `ready` 백업을 VM마다 `prune`해 남지 않는 것과, 같은 VM의 더 새(id가 큰 — UUIDv7) `ready` 백업이 있는 이 작업의 `failed` 백업을 지우기 Task로 지운다(지우지 못하면 다음 실행이 끝날 때 다시 고른다). 상태 쓰기가 revision 불일치로 실패하면 다시 읽어 바꾸거나(최대 10번) 다음 tick에 다시 한다. 리더가 바뀌는 순간 두 스케줄러가 잠깐 겹칠 수 있으나, 같은 실행의 같은 VM 백업은 위처럼 받아들이므로 중복으로 만들지 않는다. `prune`은 Proxmox 규칙이다(최신 순서는 백업 id, 기간은 만든 시각): 최신부터 `last`개를 먼저 남기고, 그다음 daily(UTC 날짜)·weekly(ISO 주)·monthly(연·월) 규칙마다 최신부터 보며 그 규칙에서 처음 보는 기간이면 그 기간을 쓴 것으로 표시하고(앞 규칙이 이미 남긴 백업도 기간은 표시) 아직 남기지 않은 백업이면 남기고 센다 — N개를 남기면 그 규칙은 끝; 남지 않은 것을 지운다.

구현(계획 28): oplctl `backup-job create NAME --pool P (--all | --vm NAME… | --label K=V…) --at HH:MM [--days mon,wed,…] [--keep-last N] [--keep-daily N] [--keep-weekly N] [--keep-monthly N] [--disabled]`(출력 "backup job N created; next run <시각> UTC"), `backup-job list`(열 `NAME POOL VMS SCHEDULE KEEP ENABLED NEXT LAST`, LAST는 `running` 또는 `<시각> UTC (K ok, F failed)`), `backup-job get`, `backup-job set NAME [같은 옵션] [--enabled true|false]`(현재 작업을 읽어 준 값만 합쳐 If-Match 없이 PATCH — 스케줄러가 status를 자주 써서), `backup-job delete`("its backups are kept"), `backup-job run NAME`(설계의 `run-now`; 기다리지 않는다 — "will run at the scheduler's next check"). oplctl에는 VM 라벨을 넣는 명령이 없다(API의 `labels`로 넣는다). 웹은 사이드바 "백업" 아래 "백업 작업" 화면(`/backup-jobs`: 목록·만들기·편집·지우기·지금 실행, 다음·마지막 실행 시각은 UTC로 적어 표시, 마지막 실행 결과와 VM별 오류), 백업 목록의 "작업" 열, watch 종류 `backup-job`, ko·en 문구. 시험: opl-types 단위(직렬화 모양, 검증 경계, `next_run`·`prune` 표), opl-api `tests/backup_jobs.rs`(API)와 `tests/backup_job_runs.rs`(`storage-it`, MockHypervisor·dir 백업 풀·작은 tick: 실행 시작·VM 고르기·노드당 동시 제한·`last_run`·보존·실패한 백업 정리·진행 중 VM 건너뛰기·놓친 실행 한 번·사용 안 함·없는 이름·실행 도중 없어진 VM·Task 없이 남은 pending 백업·상태에 적지 못한 백업 받아들이기; CI `check (ubuntu)`의 storage-it 단계에서 `backup_tasks`와 함께 돈다), 실제 시험 `crates/opld/tests/backup.rs`의 `backup_job_keeps_last_backups_of_labeled_vms`(`qemu-it`): opld 하나(`[backup] tick_secs = 1`), 디스크 있는 꺼진 VM 둘(하나만 라벨 `tier=prod`), 백업 풀. 작업 `--label tier=prod --at 00:00 --keep-last 2`를 만들고 `backup-job run`을 세 번(매번 `status.run`이 끝나기를 기다림) → 매번 `last_run.ok == 1`·오류 없음, 그 VM의 백업은 정확히 2개(가장 오래된 것과 그 디렉터리가 지워짐, 남은 것의 `spec.job`은 작업 id), 라벨 없는 VM은 백업 없음, 작업을 지워도 백업은 남는다.

알려진 틈(계획 28): 보존과 실패한 백업 정리는 실행이 끝날 때만 하므로, 보존 규칙을 줄여도 다음 실행이 끝나야 지워진다. 작업을 지우면 그 작업의 백업은 더는 보존 규칙으로 지워지지 않는다(손으로 지운다). 백업 Task에는 시간 제한이 없으므로 멈춘 Task(예: 응답 없는 NFS 위 `qemu-img`)는 그 실행을 끝나지 않게 붙잡는다(지금 실행은 409, 끄면 남은 VM만 비운다 — 작업을 지우거나 Task가 끝나야 풀린다). Task가 실패 처리됐는데 pending으로 남은 백업은 실행에서는 실패로 세지만 리소스는 남는다(보존 규칙이 건드리지 않으며 풀 지우기를 막는다 — 손으로 지운다).
```

- [ ] **Step 2: 확인**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 경고 없음

Run: `cargo test --workspace`
Expected: PASS 1011개, 실패 0

Run: `cargo test -p opld --features qemu-it --test backup`
Expected: PASS 3개(이 Mac, 실제 QEMU)

- [ ] **Step 3: 커밋**

```bash
git add .github README.md crates docs
git commit -m "test(opld): 백업 작업 E2E와 문서 — README 백업 작업, 설계 "구현(계획 28)" 메모, 하위 프로젝트 7 완료, CI storage-it에 백업 시험"
```

---

## 완료 기준 (계획 28)

- 백업 작업을 만들면 다음 실행 시각(UTC)이 정해지고, 때가 되면(또는 지금 실행) 대상 VM마다 백업이 만들어지며 노드당 동시 실행 제한을 지킨다.
- 실행이 끝나면 `last_run`(ok·failed)과 오류가 남고, 보존 규칙에서 벗어난 이 작업의 백업만 지워진다. 작업을 지워도 백업은 남는다.
- 리더가 바뀌거나 상태 쓰기가 실패해도 같은 실행에서 같은 VM의 백업을 두 번 만들지 않고, Task가 끝난 pending 백업 때문에 실행이 멈춰 있지 않는다.
- 이 Mac과 Linux CI에서 `opl-api` `backup_job_runs`(storage-it)와 `opld` `backup`(qemu-it) 시험이 통과한다.
- `cargo fmt`, `clippy -D warnings`(모든 기능), `cargo test --workspace --all-features`, `cargo deny`가 통과한다.
- `pnpm -C web typecheck`, `lint`, `test`, `build`, `audit --prod`가 통과하고, `pnpm -C web gen:api`의 결과가 커밋된 `schema.d.ts`와 같다.

## 알고 받아들인 제한

- **보존은 실행이 끝날 때만** 한다. 규칙을 줄여도 다음 실행이 끝나야 지워진다.
- **멈춘 백업 Task는 실행을 붙잡는다**(백업 Task에 시간 제한이 없다). 작업을 지우거나 Task가 끝나야 풀린다.
- **리더가 바뀌는 순간** 두 스케줄러가 잠깐 겹칠 수 있다(같은 실행의 백업은 이어받아 중복을 막는다).
- 그 밖의 제한은 설계 문서 §3과 "알려진 틈(계획 28)", README 백업 작업 절에 있다.
