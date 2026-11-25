//! 선언형 적용(계획 73 §8): `POST /apply`가 매니페스트 문서를 지금 상태와 견주어 계획(만들기·바꾸기·그대로·지우기·막힘)을
//! 세우고(`?dry_run=true`면 거기서 멈춤), 막힌 단계가 없으면 Task `apply`가 의존 차례로 지금 처리기 길을 거쳐 적용한다.
//! `GET /manifests`가 같은 꼴로 내보내고, 적용 묶음(`/apply-sets`)이 마지막 선언을 기억해 세 갈래 비교·가지치기를 맡는다.
//! 새 권한은 없다: 단계마다 그 종류·대상의 지금 권한을 본다.

mod apply;
pub(crate) mod diff;
pub(crate) mod export;
pub(crate) mod kinds;
pub(crate) mod plan;

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_cluster::ChangeContext;
use opl_types::apply_set::{AppliedObject, ApplySet, ApplySetSpec, ApplySetStatus};
use opl_types::authz::Privilege;
use opl_types::manifest::{APPLY_SET_LABEL, Manifest, ManifestKind};
use opl_types::node::DECLARATIVE;
use opl_types::resource::Resource;
use opl_types::task::{Task, TaskOp};
use serde::{Deserialize, Serialize};
use utoipa::openapi::schema::{ObjectBuilder, Schema, Type};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{IntoParams, Modify, ToSchema};
use uuid::Uuid;

pub(crate) use apply::run as run_apply;
pub use diff::Change;
pub use export::{ManifestList, ManifestQuery};

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name};
use crate::state::AppState;
use kinds::{World, adapter, check_route, readable};
use plan::StoredPlan;

/// 요청 본문 한도(문서 1 000개·8 MiB — §8.2)
const MAX_BODY: usize = 8 << 20;
/// 저장한 계획이 사는 시간(§8.3)
const PLAN_TTL: Duration = Duration::from_secs(30 * 60);
/// 한 노드가 들고 있는 계획 수(넘으면 오래된 것부터 버린다)
const MAX_PLANS: usize = 256;

/// 객체를 관리하는 도구(`orbivisor.io/managed-by` 레이블과 설정 이력의 출처)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Manager {
    Terraform,
}

/// `POST /apply`의 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplyRequest {
    /// 매니페스트 문서(1 000개까지). 저장한 계획(`plan`)을 적용할 때는 비워도 되고, 주면 계획 때와 같아야 한다.
    #[serde(default)]
    #[schema(value_type = Vec<Manifest>)]
    pub documents: Vec<serde_json::Value>,
    /// 적용 묶음 이름: 이 묶음이 마지막 선언을 기억하고(세 갈래 비교), 든 리소스에 레이블 `orbivisor.io/apply-set`을 단다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub apply_set: Option<String>,
    /// 묶음이 기억하는데 문서에 없는 리소스를 지운다(`apply_set`이 있어야 한다).
    #[serde(default)]
    pub prune: bool,
    /// 가지치기가 VM·볼륨·백업 저장소도 지운다(묶음의 `protect_data`).
    #[serde(default)]
    pub prune_data: bool,
    /// `POST /apply?dry_run=true`가 돌려준 계획 id: 그 계획을 그대로 적용한다(그사이 바뀐 것이 있으면 412 `plan_stale`).
    #[serde(default)]
    #[schema(nullable = false)]
    pub plan: Option<Uuid>,
    /// 관리하는 도구(`terraform` — 레이블 `orbivisor.io/managed-by=terraform`, 이력 출처 `terraform`)
    #[serde(default)]
    #[schema(nullable = false)]
    pub manager: Option<Manager>,
    /// 다른 관리자(Terraform 레이블·다른 적용 묶음)의 객체도 바꾸고 관리를 넘겨받는다. 없으면 그 단계가
    /// `blocked: managed_elsewhere`다.
    #[serde(default)]
    pub force_conflicts: bool,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ApplyQuery {
    /// 계획만 세우고 쓰지 않는다(권한·참조·할당량·용량·MAC/IP를 단계에 보인다).
    #[serde(default)]
    pub dry_run: bool,
}

/// 단계의 동작
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepAction {
    Create,
    Update,
    Unchanged,
    Delete,
    /// 적용할 수 없다(`problems`의 오류) — 하나라도 있으면 아무것도 하지 않는다.
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProblemLevel {
    Error,
    Warning,
}

/// 단계의 문제(오류면 막힘)
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Problem {
    /// `forbidden`·`missing_reference`·`immutable`·`protected`·`quota_exceeded`·`mac_in_use`… (처리기의 오류 코드)
    pub code: String,
    pub message: String,
    /// 칸 자리(`nics.0`·`metadata.folder`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub path: Option<String>,
    pub level: ProblemLevel,
}

impl Problem {
    pub(crate) fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        path: Option<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path,
            level: ProblemLevel::Error,
        }
    }

    pub(crate) fn warning(
        code: impl Into<String>,
        message: impl Into<String>,
        path: Option<String>,
    ) -> Self {
        Self {
            level: ProblemLevel::Warning,
            ..Self::error(code, message, path)
        }
    }

    pub(crate) fn is_error(&self) -> bool {
        self.level == ProblemLevel::Error
    }
}

/// 계획의 단계 하나
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StepView {
    /// 1부터, 적용 차례
    pub n: usize,
    pub kind: ManifestKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub folder: Option<String>,
    pub action: StepAction,
    /// 바뀌는 칸(비밀은 `(changed)`)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub diff: Vec<Change>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<Problem>,
    /// 켜진 VM의 하드웨어를 바꾼다(핫플러그 Task 또는 다음 재시작 때 적용)
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub restart: bool,
    /// 먼저 해야 하는 단계 번호
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<usize>,
}

#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct Summary {
    pub create: usize,
    pub update: usize,
    pub delete: usize,
    pub unchanged: usize,
    pub blocked: usize,
}

/// 계획(`POST /apply?dry_run=true`의 답 — 받은 노드가 30분 기억한다)
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlanView {
    pub plan: Uuid,
    /// 계획을 세운 때의 revision
    pub base_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub apply_set: Option<String>,
    pub steps: Vec<StepView>,
    pub summary: Summary,
}

/// 적용을 시작했다(Task `apply`)
#[derive(Debug, Serialize, ToSchema)]
pub struct ApplyStarted {
    pub plan: PlanView,
    #[schema(value_type = Resource<opl_types::task::TaskSpec, opl_types::task::TaskStatus>)]
    pub task: Task,
}

/// 이 노드가 기억하는 계획(§8.3 — 리더가 아니라 받은 노드)
#[derive(Default)]
pub(crate) struct Plans(Mutex<HashMap<Uuid, Arc<StoredPlan>>>);

impl Plans {
    pub(crate) fn keep(&self, plan: StoredPlan) -> Arc<StoredPlan> {
        let plan = Arc::new(plan);
        let mut plans = self.0.lock().unwrap();
        plans.retain(|_, p| p.created.elapsed() < PLAN_TTL);
        while plans.len() >= MAX_PLANS {
            let Some(oldest) = plans.values().min_by_key(|p| p.created).map(|p| p.id) else {
                break;
            };
            plans.remove(&oldest);
        }
        plans.insert(plan.id, plan.clone());
        plan
    }

    fn get(&self, id: Uuid) -> Option<Arc<StoredPlan>> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .filter(|p| p.created.elapsed() < PLAN_TTL)
            .cloned()
    }

    /// 적용 Task가 계획을 가져간다(한 번만 적용된다).
    pub(crate) fn take(&self, id: Uuid) -> Option<Arc<StoredPlan>> {
        self.0
            .lock()
            .unwrap()
            .remove(&id)
            .filter(|p| p.created.elapsed() < PLAN_TTL)
    }
}

fn manifest_invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("manifest_invalid")
}

/// 본문(8 MiB까지)을 푼다.
async fn read_request(body: Body) -> Result<(ApplyRequest, Vec<Manifest>), ApiError> {
    let bytes = axum::body::to_bytes(body, MAX_BODY).await.map_err(|_| {
        ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "manifest_too_large",
            format!("an apply request is at most {MAX_BODY} bytes; split the documents"),
        )
    })?;
    let req: ApplyRequest = serde_json::from_slice(&bytes).map_err(manifest_invalid)?;
    let docs = req
        .documents
        .iter()
        .enumerate()
        .map(|(i, d)| {
            serde_json::from_value::<Manifest>(d.clone())
                .map_err(|e| manifest_invalid(format!("documents[{i}]: {e}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if req.prune && req.apply_set.is_none() {
        return Err(ApiError::bad_request("prune needs an apply_set"));
    }
    Ok((req, docs))
}

/// 매니페스트를 지금 상태와 견주어 계획을 세우고 적용한다. 어딘가의 `system.read`(폴더 사용자도 — 볼 수 있는 것만 다룬다)와 단계마다 그 종류·대상의 지금 쓰기 권한이
/// 든다(없으면 그 단계가 `blocked: forbidden` — 볼 수 없는 리소스의 이름·폴더를 새지 않는다).
///
/// `?dry_run=true`: 계획만(200, 아무것도 쓰지 않음). 아니면 막힌 단계가 있으면 409 `plan_blocked`(`details`가 계획), 없으면
/// Task `apply`(202). `plan`을 주면 그 저장한 계획을 적용한다: 다른 사용자·다른 노드·30분이 지났으면 404
/// `plan_not_found`, 문서가 다르거나 그사이 본 리소스가 바뀌었으면 412 `plan_stale`. 문서 꼴이 틀리면 400
/// `manifest_invalid`(문서 번호·칸), 같은 리소스가 두 번이면 400 `manifest_duplicate`, 참조 고리는 400 `manifest_cycle`,
/// 아직 다루지 않는 종류는 400 `manifest_unsupported`, 본문이 8 MiB를 넘으면 413. 옛 노드가 남았으면 409
/// `cluster_upgrading`.
#[utoipa::path(post, path = "/apply", tag = "declarative",
    params(ApplyQuery),
    request_body = ApplyRequest,
    responses(
        (status = 200, body = PlanView, description = "the plan (dry run)"),
        (status = 202, body = ApplyStarted),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn apply(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Query(q): Query<ApplyQuery>,
    body: Body,
) -> Result<Response, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    state
        .authz
        .require_anywhere(&principal, Privilege::SystemRead)?;
    let (req, documents) = read_request(body).await?;
    let request = plan::Request {
        documents,
        apply_set: req.apply_set.clone(),
        prune: req.prune,
        prune_data: req.prune_data,
        manager: req.manager,
        force_conflicts: req.force_conflicts,
        ..plan::Request::default()
    };
    let stored = match req.plan {
        Some(id) => {
            if q.dry_run {
                return Err(ApiError::bad_request(
                    "give documents, not a plan, to dry_run",
                ));
            }
            let stored = state
                .plans
                .get(id)
                // 계획을 세운 사용자의 같은 자격(세션 또는 같은 API 토큰)만: 단계는 세운 주체의 권한으로 돈다(리뷰 1 —
                // 좁은 토큰이 넓은 세션의 계획을 쓰지 못하게).
                .filter(|p| p.owner.user == principal.user && p.owner.token == principal.token)
                .ok_or_else(|| {
                    ApiError::not_found(format!(
                        "plan {id} not found on this node (plans live 30 minutes on the node that made them)"
                    ))
                    .with_code("plan_not_found")
                })?;
            if !request.documents.is_empty() && plan::hash(&request) != stored.hash {
                return Err(ApiError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "plan_stale",
                    "the documents differ from the plan's; plan again",
                ));
            }
            plan::still_valid(&state, &stored)?;
            stored
        }
        None => {
            let meta = ChangeContext::current().unwrap_or_default();
            let made = Box::pin(plan::make_plan(&state, &principal, &request, meta)).await?;
            let stored = state.plans.keep(made);
            if q.dry_run {
                return Ok(Json(stored.view.clone()).into_response());
            }
            stored
        }
    };
    let started = start(&state, &stored).await?;
    let s = &stored.view.summary;
    audit.detail(format!(
        "apply ({}{} create, {} update, {} delete, plan {})",
        stored
            .apply_set
            .as_ref()
            .map_or_else(String::new, |n| format!("set {n}: ")),
        s.create,
        s.update,
        s.delete,
        stored.id
    ));
    Ok((StatusCode::ACCEPTED, Json(started)).into_response())
}

/// 계획을 적용한다(Task `apply`): 막힌 단계가 있으면 409 `plan_blocked`(`details`가 계획)이고 아무것도 하지 않는다.
/// 되돌리기(`/history/…/restore`)도 이 길로 적용한다.
pub(crate) async fn start(state: &AppState, stored: &StoredPlan) -> Result<ApplyStarted, ApiError> {
    if stored.blocked() {
        let mut e = ApiError::conflict(
            "plan_blocked",
            format!(
                "{} step(s) are blocked; nothing was applied (see details.steps)",
                stored.view.summary.blocked
            ),
        );
        e.details = Some(serde_json::to_value(&stored.view).expect("plan"));
        return Err(e);
    }
    let set = match &stored.apply_set {
        Some(name) => Some(ensure_set(state, name).await?),
        None => None,
    };
    let task = crate::tasks::create_task(
        &state.cluster,
        TaskOp::Apply {
            set,
            plan: stored.id,
        },
        set.unwrap_or(stored.id),
        state.node,
    )
    .await?;
    Ok(ApplyStarted {
        plan: stored.view.clone(),
        task,
    })
}

/// 적용 묶음(없으면 만든다)의 id
async fn ensure_set(state: &AppState, name: &str) -> Result<Uuid, ApiError> {
    check_name(name)?;
    if let Some(set) = state.cluster.get_by_name::<ApplySetSpec>(name)? {
        return Ok(set.id);
    }
    let mut set: ApplySet = Resource::new(name, ApplySetSpec::default());
    set.status = ApplySetStatus::default();
    match state.cluster.create(&set).await {
        Ok(_) => Ok(set.id),
        // 다른 적용이 그새 만들었다.
        Err(opl_cluster::ClusterError::Rejected(opl_cluster::types::Rejection::NameTaken {
            ..
        })) => state
            .cluster
            .get_by_name::<ApplySetSpec>(name)?
            .map(|s| s.id)
            .ok_or_else(|| ApiError::conflict("busy", "the apply set changed; try again")),
        Err(e) => Err(e.into()),
    }
}

/// 지금 클러스터를 매니페스트로 내보낸다(적용 차례, 볼 수 있는 것만, 비밀은 `secretRef`). 이 결과를 그대로 같은 클러스터에
/// 적용하면 모든 단계가 `unchanged`다.
#[utoipa::path(get, path = "/manifests", tag = "declarative",
    params(ManifestQuery),
    responses((status = 200, body = ManifestList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_manifests(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ManifestQuery>,
) -> Result<Json<ManifestList>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    Ok(Json(export::list(&state, &principal, &q)?))
}

/// 리소스 하나의 매니페스트(`kind`는 `vm`·`Vm` 꼴, `key`는 id 또는 이름 — 폴더는 id 또는 `트리:경로`). 볼 수 없으면 404.
#[utoipa::path(get, path = "/manifests/{kind}/{key}", tag = "declarative",
    params(("kind" = String, Path, description = "manifest kind"), ("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Manifest), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_manifest(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((kind, key)): Path<(String, String)>,
) -> Result<Json<Manifest>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    Ok(Json(export::one(&state, &principal, &kind, &key)?))
}

/// `DELETE /manifests/{kind}/{key}`의 답
#[derive(Debug, Serialize, ToSchema)]
pub struct ManifestDeleted {
    /// 지우기가 만든 Task(볼륨 — 끝날 때까지 기다리지 않는다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub task: Option<Uuid>,
}

/// 리소스 하나를 그 종류의 지금 지우기 길로 지운다(경로표의 지우기 권한 — Terraform 공급자의 Delete). 켜진 VM은 먼저
/// 끈다(600초까지). 볼 수 없으면 404, 처리기의 거절(쓰는 VM이 있는 네트워크 등)은 그대로.
#[utoipa::path(delete, path = "/manifests/{kind}/{key}", tag = "declarative",
    params(("kind" = String, Path, description = "manifest kind"), ("key" = String, Path, description = "id or name")),
    responses((status = 200, body = ManifestDeleted), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_manifest(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path((kind, key)): Path<(String, String)>,
) -> Result<Json<ManifestDeleted>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    let kind = export::parse_kind(&kind)?;
    let world = World::load(&state.cluster)?;
    let live = export::find(&state, &principal, &world, kind, &key)?;
    let a = adapter(kind).ok_or_else(|| ApiError::internal("no adapter"))?;
    if !a.deletable() {
        return Err(ApiError::bad_request(format!(
            "{} documents cannot be deleted",
            kind.as_str()
        )));
    }
    check_route(&state, &principal, a.delete_route(), Some(live.id))?;
    let done = Box::pin(a.delete(&state, &principal, &live)).await?;
    audit.detail(format!("manifest delete {} {}", kind.as_str(), live.name));
    Ok(Json(ManifestDeleted { task: done.task }))
}

/// 적용 묶음에서 볼 수 없는 객체를 뺀다(§13 — 묶음 안 객체는 보이는 것만). 밖에서 지운 객체(드리프트 `missing`)는
/// 마지막으로 알던 폴더로 보고, 볼 수 있으면 `status.objects`에도 남긴다(리뷰 2 — 화면이 받아들이기에 고를 수 있게).
pub(crate) fn visible_set(
    state: &AppState,
    principal: &Principal,
    mut set: ApplySet,
) -> Result<ApplySet, ApiError> {
    let world = World::load(&state.cluster)?;
    let mut owners = None;
    let mut visible = |o: &AppliedObject| -> Result<bool, ApiError> {
        let Some(k) = ManifestKind::from_kind(o.kind) else {
            return Ok(false);
        };
        Ok(match world.by_id(k, o.id) {
            Some(l) => readable(state, principal, l),
            None => {
                if owners.is_none() {
                    owners = Some(crate::authz::Owners::load(&state.cluster)?);
                }
                let owners = owners.as_ref().expect("just loaded");
                kinds::gone_readable(state, principal, owners, &world, o)
            }
        })
    };
    let mut objects = Vec::with_capacity(set.status.objects.len());
    let mut hidden = BTreeSet::new();
    for o in std::mem::take(&mut set.status.objects) {
        if visible(&o)? {
            objects.push(o);
        } else {
            hidden.insert((o.kind, o.id));
        }
    }
    set.status.objects = objects;
    if let Some(drift) = set.status.drift.as_mut() {
        drift.items.retain(|i| !hidden.contains(&(i.kind, i.id)));
    }
    Ok(set)
}

/// 적용 묶음 목록(루트 `system.read`, 묶음 안 객체는 볼 수 있는 것만)
#[utoipa::path(get, path = "/apply-sets", tag = "declarative",
    responses((status = 200, body = List<Resource<ApplySetSpec, ApplySetStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_apply_sets(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<ApplySet>>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    let (revision, sets) = state.cluster.list::<ApplySetSpec>()?;
    let items = sets
        .into_iter()
        .map(|s| visible_set(&state, &principal, s))
        .collect::<Result<_, _>>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/apply-sets/{key}", tag = "declarative",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<ApplySetSpec, ApplySetStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_apply_set(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<ApplySet>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    let set = crate::resources::lookup::<ApplySetSpec>(&state.cluster, &key)?;
    Ok(Json(visible_set(&state, &principal, set)?))
}

/// 적용 묶음을 지운다: 리소스는 지우지 않고 레이블 `orbivisor.io/apply-set`만 뗀다. 묶음이 기억하는 객체 모두의 바꾸기
/// 권한이 든다(루트 `system.read`도 — 하나라도 없으면 403, 아무것도 바꾸지 않음).
#[utoipa::path(delete, path = "/apply-sets/{key}", tag = "declarative",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_apply_set(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    state
        .authz
        .require_anywhere(&principal, Privilege::SystemRead)?;
    let set = crate::resources::lookup::<ApplySetSpec>(&state.cluster, &key)?;
    let world = World::load(&state.cluster)?;
    let mut labeled = Vec::new();
    for o in &set.status.objects {
        let Some(live) = ManifestKind::from_kind(o.kind).and_then(|k| world.by_id(k, o.id)) else {
            continue;
        };
        if live.labels.get(APPLY_SET_LABEL) != Some(&set.name) {
            continue;
        }
        let a = adapter(live.kind).ok_or_else(|| ApiError::internal("no adapter"))?;
        check_route(&state, &principal, a.update_route(), Some(live.id))
            .map_err(|_| ApiError::forbidden())?;
        labeled.push(live);
    }
    for live in &labeled {
        let a = adapter(live.kind).ok_or_else(|| ApiError::internal("no adapter"))?;
        let mut labels = live.labels.clone();
        labels.remove(APPLY_SET_LABEL);
        a.write_labels(&state.cluster, live.id, live.revision, labels)
            .await?;
    }
    state.cluster.delete::<ApplySetSpec>(set.id, None).await?;
    audit.detail(format!(
        "apply-set delete {} ({} labels removed)",
        set.name,
        labeled.len()
    ));
    Ok(StatusCode::NO_CONTENT)
}

/// 스키마 `T`와 그것이 가리키는 스키마를 없으면 더한다.
fn register<T: ToSchema>(components: &mut utoipa::openapi::Components) {
    let mut all = vec![(T::name().into_owned(), T::schema())];
    T::schemas(&mut all);
    for (name, schema) in all {
        components.schemas.entry(name).or_insert(schema);
    }
}

/// OpenAPI에 종류마다 매니페스트 꼴 `Manifest{Kind}`(spec은 그 종류의 저장 spec — 폴더는 `metadata.folder` 경로)를 낸다
/// (Terraform 공급자의 스키마 맞춤 시험·웹 타입이 쓴다 — §13).
pub(crate) struct ManifestSchemas;

impl Modify for ManifestSchemas {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        // 처리기가 보기 꼴만 내는 종류의 저장 spec(매니페스트 spec이 가리킨다)
        register::<opl_types::backup_store::BackupStoreSpec>(components);
        register::<opl_types::alert::NotificationTargetSpec>(components);
        // 역할 spec은 API 본문에 나오지 않는다(보기 `AccessRoleView`) — 매니페스트가 가리키도록 낸다.
        components
            .schemas
            .entry("AccessRoleSpec".into())
            .or_insert_with(<opl_types::authz::AccessRoleSpec as utoipa::PartialSchema>::schema);
        // grant 매니페스트 spec은 저장 꼴이 아니라 이름으로 적는다(`kinds/grant.rs`).
        let text = || ObjectBuilder::new().schema_type(Type::String);
        components.schemas.insert(
            "ManifestGrantSpec".into(),
            RefOr::T(Schema::Object(
                ObjectBuilder::new()
                    .property("user", text().description(Some("user name (or group)")))
                    .property("group", text().description(Some("group name (or user)")))
                    .property(
                        "role",
                        text().description(Some("built-in role (admin, operator, viewer) or access role name")),
                    )
                    .required("role")
                    .property(
                        "target",
                        text().description(Some(
                            "\"/\", a folder \"vm:/path\" (vm, network, storage) or an object \"vm/name\"",
                        )),
                    )
                    .required("target")
                    .property(
                        "propagate",
                        ObjectBuilder::new()
                            .schema_type(Type::Boolean)
                            .default(Some(serde_json::Value::Bool(true))),
                    )
                    .build(),
            )),
        );
        for kind in kinds::ORDER {
            let spec = match kind {
                ManifestKind::Vm => "VmSpec",
                ManifestKind::Volume => "VolumeSpec",
                ManifestKind::Network => "NetworkSpec",
                ManifestKind::Router => "RouterSpec",
                ManifestKind::FloatingIp => "FloatingIpSpec",
                ManifestKind::SecurityGroup => "SecurityGroupSpec",
                ManifestKind::StoragePool => "StoragePoolSpec",
                ManifestKind::Folder => "FolderSpec",
                ManifestKind::PlacementRule => "PlacementRuleSpec",
                ManifestKind::BackupStore => "BackupStoreSpec",
                // 저장 spec에 스키마가 없다(또는 서버 칸 `ca_pem`이 있다) — 같은 칸의 처리기 본문
                ManifestKind::Remote => "RemoteRequest",
                ManifestKind::BackupJob => "BackupJobSpec",
                ManifestKind::Realm => "RealmRequest",
                ManifestKind::NotificationTarget => "NotificationTargetSpec",
                ManifestKind::AlertRule => "AlertRuleSpec",
                ManifestKind::Settings => "SettingsSpec",
                ManifestKind::LoadBalancer => "LoadBalancerSpec",
                ManifestKind::HaGroup => "HaGroupSpec",
                ManifestKind::HaCheck => "HaCheckSpec",
                ManifestKind::AccessRole => "AccessRoleSpec",
                ManifestKind::Group => "GroupSpec",
                ManifestKind::Grant => "ManifestGrantSpec",
                ManifestKind::AcmeAccount => "AcmeAccountSpec",
                ManifestKind::AcmeDns => "AcmeDnsSpec",
                ManifestKind::Certificate => "CertificateSpec",
            };
            let mut object = ObjectBuilder::new()
                .property(
                    "apiVersion",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .enum_values(Some([opl_types::manifest::API_VERSION])),
                )
                .required("apiVersion")
                .property(
                    "kind",
                    ObjectBuilder::new()
                        .schema_type(Type::String)
                        .enum_values(Some([kind.as_str()])),
                )
                .required("kind")
                .property("metadata", Ref::from_schema_name("ManifestMetadata"))
                .required("metadata")
                .property("spec", Ref::from_schema_name(spec))
                .required("spec");
            if kind == ManifestKind::Vm {
                object = object.property("state", Ref::from_schema_name("ManifestState"));
            }
            components.schemas.insert(
                format!("Manifest{}", kind.as_str()),
                RefOr::T(Schema::Object(object.build())),
            );
        }
    }
}
