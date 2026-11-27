//! Task `apply`(계획 73 §8.3): 저장한 계획의 단계를 차례로 — 권한 다시 보기 → 계획 때 revision(`If-Match`) → 지금
//! 처리기로 쓰기 → 그 쓰기가 만든 Task 기다리기. 첫 실패에서 멈추고 성공한 단계는 되돌리지 않는다(다시 적용하면 남은 것만
//! 한다). 몇 단계마다(리뷰 2 — [`RECORD_EVERY`]·[`RECORD_AFTER`]), 그리고 끝나면(실패해도) 적용 묶음 status에 성공한
//! 단계까지의 선언을 남긴다. 받은 노드에서만 돌고 이어
//! 가지 않는다.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use orbivisor_cluster::types::Rejection;
use orbivisor_cluster::{ChangeContext, Cluster, ClusterError};
use orbivisor_types::apply_set::{AppliedObject, ApplySetSpec, ApplySetStatus};
use orbivisor_types::audit::AuditTarget;
use orbivisor_types::history::ChangeSource;
use orbivisor_types::manifest::{ManifestKind, folder_path};
use orbivisor_types::resource::Kind;
use orbivisor_types::task::{Task, TaskProgress, TaskSpec, TaskState, TaskStatus};
use uuid::Uuid;

use super::Manager;
use super::StepAction;
use super::kinds::{Planned, World, adapter, check_route, tree_of_doc};
use super::plan::{Step, StoredPlan};
use crate::auth::Principal;
use crate::error::ApiError;
use crate::state::AppState;
use crate::tasks::failure::{coded, coded_over};
use orbivisor_types::task_failure::{TaskFailure, code};

/// 쓰기가 만든 Task를 다시 보는 간격
const POLL: Duration = Duration::from_millis(100);
/// 적용 묶음 진행 기록: 이만큼의 단계마다 또는 이 시간이 지나면(그리고 끝에 늘). 단계마다 쓰면 묶음 전체(객체 1 000개·
/// 4 MiB까지)를 단계 수만큼 Raft에 쓴다(리뷰 2).
const RECORD_EVERY: usize = 50;
const RECORD_AFTER: Duration = Duration::from_secs(5);

/// 진행 기록 때를 센다.
struct Pace {
    since: usize,
    last: std::time::Instant,
}

impl Pace {
    fn new() -> Self {
        Self {
            since: 0,
            last: std::time::Instant::now(),
        }
    }

    /// 단계 하나가 끝났다: 지금 기록할 때인지
    fn step(&mut self) -> bool {
        self.since += 1;
        if self.since >= RECORD_EVERY || self.last.elapsed() >= RECORD_AFTER {
            self.since = 0;
            self.last = std::time::Instant::now();
            true
        } else {
            false
        }
    }
}

/// Task `apply`를 돈다(Task 실행기가 부른다).
pub(crate) async fn run(
    api: Option<Arc<AppState>>,
    task: &Task,
    plan: Uuid,
) -> Result<Option<String>, String> {
    let state = api.ok_or("this node has no API to apply with")?;
    let plan = state.plans.take(plan).ok_or_else(|| {
        coded(
            TaskFailure::new(code::APPLY_PLAN_EXPIRED),
            "the plan is gone (it expired or the node restarted); apply again",
        )
    })?;
    let mut meta = plan.meta.clone();
    meta.actor.task = Some(task.id);
    // 되돌리기는 요청한 곳의 출처(웹·CLI)를 그대로 둔다 — 이력 줄이 `restored_from`으로 되돌리기임을 보인다.
    if meta.restored_from.is_none() {
        meta.source = match plan.manager {
            Some(Manager::Terraform) => ChangeSource::Terraform,
            None => ChangeSource::Apply,
        };
    }
    meta.apply_set = match &plan.apply_set {
        Some(name) => state
            .cluster
            .get_by_name::<ApplySetSpec>(name)
            .ok()
            .flatten()
            .map(|s| s.id),
        None => None,
    };
    let owner = plan.owner.clone();
    // 처리기가 Task를 만든 사용자·SDN 주소 규칙의 주체를 찾도록 요청 안처럼, 쓰기는 적용(Terraform) 출처로
    ChangeContext::new(meta)
        .scope(crate::audit::as_principal(
            owner,
            Box::pin(execute(&state, task.id, &plan)),
        ))
        .await
        .map(|()| None)
}

/// 유닉스 밀리초(`ApplySetStatus.applied_at`의 단위 — 계획 77-7 전에는 초로 적었다, 웹이 둘 다 읽는다)
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn verb(action: StepAction) -> &'static str {
    match action {
        StepAction::Create => "create",
        StepAction::Update => "update",
        StepAction::Delete => "delete",
        StepAction::Unchanged => "keep",
        StepAction::Blocked => "blocked",
    }
}

async fn execute(state: &Arc<AppState>, task: Uuid, plan: &StoredPlan) -> Result<(), String> {
    let total = plan
        .steps
        .iter()
        .filter(|s| {
            matches!(
                s.action,
                StepAction::Create | StepAction::Update | StepAction::Delete
            )
        })
        .count();
    let mut world = World::load(&state.cluster).map_err(|e| e.message)?;
    let mut applied: Vec<AppliedObject> = Vec::new();
    let mut removed: BTreeSet<(Kind, Uuid)> = BTreeSet::new();
    let mut failure = None;
    let mut done = 0;
    let mut pace = Pace::new();
    for step in &plan.steps {
        if step.action == StepAction::Unchanged {
            if let Some((id, revision)) = step.seen {
                applied.push(AppliedObject {
                    kind: step.kind.kind(),
                    id,
                    name: step.name.clone(),
                    declared: step.declared.clone(),
                    revision,
                });
            }
            continue;
        }
        done += 1;
        progress(
            &state.cluster,
            task,
            &format!(
                "{done}/{total} {} {} {}",
                verb(step.action),
                step.kind.as_str(),
                step.name
            ),
            done as u64,
            total as u64,
        )
        .await;
        match Box::pin(run_step(state, &plan.owner, &mut world, step)).await {
            Ok(Some(obj)) => {
                record(state, &plan.owner, plan.id, step, None);
                applied.push(obj);
                if pace.step() {
                    record_progress(state, plan, &applied, &removed).await;
                }
            }
            Ok(None) => {
                record(state, &plan.owner, plan.id, step, None);
                if let Some((id, _)) = step.seen {
                    removed.insert((step.kind.kind(), id));
                }
                if pace.step() {
                    record_progress(state, plan, &applied, &removed).await;
                }
            }
            Err(e) => {
                record(state, &plan.owner, plan.id, step, Some(&e.message));
                let what = format!(
                    "{} {} {:?}",
                    verb(step.action),
                    step.kind.as_str(),
                    step.name
                );
                // 계획을 만든 뒤 대상이 바뀌었으면 충돌, 그 밖은 단계 실패(서버 오류 코드는 `cause`) — 계획 77-2.
                // 처리기가 안에서 단 코드(예: 저장소에 닿지 않음)는 버린다: 단계·항목을 잃고, 만들다 실패한 항목은 아직
                // 화면에 없다(리뷰 I6).
                let why = if e.code == "revision_mismatch" {
                    code::APPLY_CONFLICT
                } else {
                    code::APPLY_STEP_FAILED
                };
                failure = Some(coded_over(
                    TaskFailure::new(why)
                        .param("step", format!("{done}/{total}"))
                        .param("action", verb(step.action))
                        .param("kind", step.kind.as_str())
                        .param("name", &step.name)
                        .param("cause", e.code)
                        .param("reason", &e.message)
                        .step(format!("step {done}/{total} ({what})")),
                    format!("step {done}/{total} ({what}) failed: {}", e.message),
                ));
                break;
            }
        }
    }
    if let Some(name) = &plan.apply_set
        && let Err(e) = record_set(state, plan, name, applied, &removed, failure.clone()).await
    {
        let e = format!("cannot record apply set {name:?}: {}", e.message);
        return Err(failure.map_or(e.clone(), |f| format!("{f}; {e}")));
    }
    match failure {
        Some(f) => Err(f),
        None => Ok(()),
    }
}

/// 적용 묶음 status에 여기까지를 남긴다(리뷰 1 — 노드가 도중에 다시 시작해 Task가 "계획이 없다"로 끝나도 만든 객체를
/// 묶음이 안다: 드리프트·가지치기·묶음 지우기). [`RECORD_EVERY`] 단계 또는 [`RECORD_AFTER`]마다, 끝에(실패해도) 늘
/// 쓴다(리뷰 2). 그 사이에 노드가 멈추면 마지막 기록 뒤에 만든 것은 묶음 레이블만 달고 있다 — 같은 파일을 다시
/// 적용하면 그대로(`unchanged`)로 찾아 다시 기록한다. 상태 쓰기라 이력 줄이 없다. 못 쓰면 끝에서 다시 쓴다.
async fn record_progress(
    state: &AppState,
    plan: &StoredPlan,
    applied: &[AppliedObject],
    removed: &BTreeSet<(Kind, Uuid)>,
) {
    let Some(name) = &plan.apply_set else {
        return;
    };
    if let Err(e) = record_set(state, plan, name, applied.to_vec(), removed, None).await {
        tracing::warn!(set = %name, error = %e.message, "cannot record the apply progress");
    }
}

/// 단계 하나의 감사 줄(같은 계획 id로 묶인다)
fn record(state: &AppState, owner: &Principal, plan: Uuid, step: &Step, error: Option<&str>) {
    let mut detail = format!(
        "apply {} {} {} (plan {plan})",
        verb(step.action),
        step.kind.as_str(),
        step.name
    );
    if let Some(e) = error {
        detail.push_str(&format!(": {e}"));
    }
    let mut entry = crate::audit::system_entry(
        &format!("apply.{}", verb(step.action)),
        AuditTarget {
            kind: step.kind.kind().as_str().into(),
            key: step.name.clone(),
        },
        detail,
    );
    entry.user = Some(owner.name.clone());
    entry.user_id = Some(owner.user);
    entry.role = Some(owner.role);
    state.audit.record(entry);
}

/// 계획 때 본 객체를 다시 찾는다(설정 — 이름·레이블·spec — 이 바뀌었으면 412: 다시 계획하라). 상태만 바뀐 것은 지금
/// revision으로 쓴다(리뷰 1 — 조정기의 상태 쓰기가 긴 적용을 깨지 않게).
fn seen_live(state: &AppState, step: &Step) -> Result<super::kinds::Live, ApiError> {
    let (id, revision) = step
        .seen
        .ok_or_else(|| ApiError::internal("a step without an object"))?;
    let a = adapter(step.kind).ok_or_else(|| ApiError::internal("no adapter"))?;
    // 세계(앞 단계 뒤에 읽음)가 아니라 지금 값 — 그 뒤 상태 쓰기로 오른 revision을 `If-Match`에 쓴다.
    let live = a
        .lives(&state.cluster)?
        .into_iter()
        .find(|l| l.id == id)
        .ok_or_else(|| {
            ApiError::not_found(format!("{} {:?} is gone", step.kind.as_str(), step.name))
        })?;
    if live.config_fingerprint() != step.seen_config {
        return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
            expected: revision,
            actual: live.revision,
        })
        .into());
    }
    Ok(live)
}

/// 문서의 폴더를 지금 id로(앞 단계가 만들었다)
fn folder_now(world: &World, step: &Step) -> Result<Option<Uuid>, ApiError> {
    let Some(doc) = &step.doc else {
        return Ok(None);
    };
    let tree = tree_of_doc(doc).map_err(ApiError::bad_request)?;
    match (tree, doc.metadata.folder.as_deref()) {
        (Some(tree), Some(path))
            if !folder_path(path)
                .map_err(|e| ApiError::bad_request(e.to_string()))?
                .is_empty() =>
        {
            world
                .folder_id(tree, path)
                .map(Some)
                .ok_or_else(|| ApiError::not_found(format!("folder {path:?} not found")))
        }
        _ => Ok(None),
    }
}

/// 단계 하나. 만들기·바꾸기는 묶음에 남길 객체, 지우기는 None.
async fn run_step(
    state: &Arc<AppState>,
    owner: &Principal,
    world: &mut World,
    step: &Step,
) -> Result<Option<AppliedObject>, ApiError> {
    let a = adapter(step.kind).ok_or_else(|| ApiError::internal("no adapter"))?;
    let id = match step.action {
        StepAction::Create => {
            if world.objects.contains_key(&(step.kind, step.key.clone())) {
                return Err(ApiError::conflict(
                    "name_taken",
                    format!(
                        "{} {:?} was made after the plan; plan again",
                        step.kind.as_str(),
                        step.name
                    ),
                ));
            }
            let doc = step
                .doc
                .as_ref()
                .ok_or_else(|| ApiError::internal("no document"))?;
            let folder = folder_now(world, step)?;
            let mut store = a
                .to_store(&doc.spec, &step.name, folder, doc.state, world)
                .map_err(ApiError::bad_request)?;
            a.create_defaults(state, world, &Planned::default(), &mut store)?;
            a.may_create(state, owner, &store, folder)?;
            let labels = if a.create_carries_labels() {
                step.labels.clone()
            } else {
                Default::default()
            };
            let create = a.create(state, owner, step.name.clone(), labels, store);
            let done = match step.reuse_id {
                // 지운 것 살리기: 처리기의 만들기 한 번이 옛 id를 쓴다(§12.3).
                Some(id) => {
                    let (done, used) =
                        orbivisor_cluster::change::reuse_id(step.kind.kind(), id, create).await;
                    let mut done = done?;
                    if used {
                        done.id = id;
                    }
                    done
                }
                None => create.await?,
            };
            if let Some(t) = done.task {
                wait_task(&state.cluster, t).await?;
            }
            if !a.create_carries_labels() && !step.labels.is_empty() {
                let revision = current_revision(state, step.kind, done.id)?;
                a.write_labels(&state.cluster, done.id, revision, step.labels.clone())
                    .await?;
            }
            done.id
        }
        StepAction::Update => {
            let live = seen_live(state, step)?;
            check_route(state, owner, a.update_route(), Some(live.id))?;
            a.may_update_extra(state, owner, &live)?;
            if step.spec_changed {
                let doc = step
                    .doc
                    .as_ref()
                    .ok_or_else(|| ApiError::internal("no document"))?;
                let folder = folder_now(world, step)?;
                let mut store = a
                    .to_store(&doc.spec, &live.name, folder, doc.state, world)
                    .map_err(ApiError::bad_request)?;
                a.keep_server_fields(state, &step.declared_spec, &mut store, &live.spec);
                let desired = a.normalize(&store).map_err(ApiError::bad_request)?;
                let labels = if a.update_carries_labels() {
                    step.labels.clone()
                } else {
                    live.labels.clone()
                };
                let done = a
                    .update(state, owner, &live, labels, desired, step.fields.clone())
                    .await?;
                if let Some(t) = done.task {
                    wait_task(&state.cluster, t).await?;
                }
            }
            if step.labels_changed && !(step.spec_changed && a.update_carries_labels()) {
                let revision = current_revision(state, step.kind, live.id)?;
                a.write_labels(&state.cluster, live.id, revision, step.labels.clone())
                    .await?;
            }
            live.id
        }
        StepAction::Delete => {
            let live = seen_live(state, step)?;
            check_route(state, owner, a.delete_route(), Some(live.id))?;
            let done = a.delete(state, owner, &live).await?;
            if let Some(t) = done.task {
                wait_task(&state.cluster, t).await?;
            }
            *world = World::load(&state.cluster)?;
            return Ok(None);
        }
        StepAction::Unchanged | StepAction::Blocked => {
            return Err(ApiError::internal("not a step to run"));
        }
    };
    *world = World::load(&state.cluster)?;
    let revision = current_revision(state, step.kind, id)?;
    Ok(Some(AppliedObject {
        kind: step.kind.kind(),
        id,
        name: step.name.clone(),
        declared: step.declared.clone(),
        revision,
    }))
}

fn current_revision(state: &AppState, kind: ManifestKind, id: Uuid) -> Result<u64, ApiError> {
    let a = adapter(kind).ok_or_else(|| ApiError::internal("no adapter"))?;
    a.lives(&state.cluster)?
        .into_iter()
        .find(|l| l.id == id)
        .map(|l| l.revision)
        .ok_or_else(|| ApiError::not_found(format!("{} {id} is gone", kind.as_str())))
}

/// 쓰기가 만든 Task가 끝나기를 기다린다(실패면 그 오류).
pub(crate) async fn wait_task(cluster: &Cluster, id: Uuid) -> Result<(), ApiError> {
    loop {
        match cluster.get::<TaskSpec>(id)? {
            None => return Err(ApiError::not_found(format!("task {id} is gone"))),
            Some(t) if t.status.state == TaskState::Succeeded => return Ok(()),
            Some(t) if t.status.state == TaskState::Failed => {
                return Err(ApiError::conflict(
                    "task_failed",
                    format!(
                        "task {id} ({}) failed: {}",
                        serde_json::to_value(&t.spec.op)
                            .ok()
                            .and_then(|v| v["kind"].as_str().map(str::to_string))
                            .unwrap_or_default(),
                        t.status.error.unwrap_or_default()
                    ),
                ));
            }
            Some(_) => tokio::time::sleep(POLL).await,
        }
    }
}

/// 진행(`n/m 동작 종류 이름`). 실패는 경고만.
async fn progress(cluster: &Cluster, task: Uuid, stage: &str, done: u64, total: u64) {
    let progress = TaskProgress {
        stage: stage.into(),
        done_bytes: done,
        total_bytes: total,
    };
    for _ in 0..5 {
        let Ok(Some(current)) = cluster.get::<TaskSpec>(task) else {
            return;
        };
        if current.status.state != TaskState::Running {
            return;
        }
        let status = TaskStatus::running(Some(progress.clone()), Some(&current.status));
        match cluster
            .update_status_checked::<TaskSpec>(task, current.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => {
                tracing::warn!(task = %task, error = %e, "cannot record the apply progress");
                return;
            }
            Ok(_) => return,
        }
    }
}

/// 적용 묶음 status: 이번에 성공했거나 그대로인 것 + 앞 선언 가운데 이번에 다루지 못한 것(지운 것은 뺀다).
async fn record_set(
    state: &AppState,
    plan: &StoredPlan,
    name: &str,
    applied: Vec<AppliedObject>,
    removed: &BTreeSet<(Kind, Uuid)>,
    failure: Option<String>,
) -> Result<(), ApiError> {
    for _ in 0..10 {
        let Some(set) = state.cluster.get_by_name::<ApplySetSpec>(name)? else {
            return Err(ApiError::not_found(format!("apply set {name:?} not found")));
        };
        let mut objects: Vec<AppliedObject> = set
            .status
            .objects
            .iter()
            .filter(|o| {
                !removed.contains(&(o.kind, o.id))
                    && !applied.iter().any(|a| a.kind == o.kind && a.id == o.id)
            })
            .cloned()
            .collect();
        objects.extend(applied.iter().cloned());
        let status = ApplySetStatus {
            applied_at: Some(now_ms()),
            applied_by: Some(plan.owner.name.clone()),
            applied_revision: state.cluster.revision(),
            objects,
            drift: set.status.drift.clone(),
            last_error: failure.clone(),
        };
        if let Err(e) = status.check_limits() {
            return Err(ApiError::bad_request(e.to_string()).with_code("apply_set_too_large"));
        }
        match state
            .cluster
            .update_status_checked::<ApplySetSpec>(set.id, set.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the apply set kept changing; apply again",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 리뷰 2: 진행 기록은 단계마다가 아니라 [`RECORD_EVERY`] 단계마다(또는 [`RECORD_AFTER`]가 지나면)다.
    #[test]
    fn progress_is_recorded_every_few_steps() {
        let mut pace = Pace::new();
        let due: Vec<usize> = (1..=2 * RECORD_EVERY + 3).filter(|_| pace.step()).collect();
        assert_eq!(due, [RECORD_EVERY, 2 * RECORD_EVERY]);
        pace.last -= RECORD_AFTER;
        assert!(pace.step(), "time also makes it due");
        assert!(!pace.step());
    }
}
