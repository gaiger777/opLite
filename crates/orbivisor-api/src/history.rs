//! 설정 이력 보기·되돌리기(계획 73 §12.1~12.4): 타임라인(`GET /history`), 리소스 하나의 줄과 revision 사이 비교,
//! 하나 되돌리기·지운 것 살리기(같은 id)·한 시점으로 여러 개 되돌리기. 되돌리기는 모두 이력 줄의 spec을 매니페스트로 바꿔
//! 선언형 계획(`declarative::plan::make_plan`)을 세우고 Task `apply`로 적용한다 — 권한·미리 확인·의존 차례·저장한 계획이
//! 그대로다. 비밀은 이력에 평문이 없어 되돌리지 않는다(지금 값이 남는다).
//!
//! 이력 표는 Raft 구성원의 상태 머신에만 있다. 관찰 노드의 API는 구성원에게 `ReadHistory` RPC로 읽는다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use orbivisor_cluster::ChangeContext;
use orbivisor_cluster::store::NodeMode;
use orbivisor_cluster::types::{NodeId, RawResource};
use orbivisor_peer::history::{HistoryRequest, MAX_HISTORY_READ};
use orbivisor_types::apply_set::ApplySetSpec;
use orbivisor_types::authz::Privilege;
use orbivisor_types::folder::{FolderKind, FolderSpec};
use orbivisor_types::history::{
    Actor, ChangeMeta, ChangeSource, HistoryOp, HistoryRecord, SECRET_MARK, clean_reason,
};
use orbivisor_types::manifest::{APPLY_SET_LABEL, Manifest, ManifestKind, SecretValue};
use orbivisor_types::node::{CONFIG_HISTORY, DECLARATIVE};
use orbivisor_types::resource::Kind;
use orbivisor_types::storage::VolumeSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::{AuditNote, parse_time};
use crate::auth::{Action, Principal};
use crate::authz::Owners;
use crate::cluster::require_feature;
use crate::declarative::diff::{Change, diff};
use crate::declarative::kinds::{Live, World, adapter};
use crate::declarative::plan::{self, StoredPlan, summarize};
use crate::declarative::{
    ApplyStarted, PlanView, Problem, StepAction, StepView, export::parse_kind, start,
};
use crate::error::{ApiError, Json, Query};
use crate::state::AppState;

/// 타임라인 기본·최대 줄 수(§12.1)
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1_000;
/// 한 요청이 거르며 훑는 줄 수 한도(넘으면 `next_before`로 이어 읽는다)
const MAX_SCAN: usize = 50_000;
/// 줄마다 보이는 바뀐 경로 수
const MAX_CHANGED: usize = 20;
/// 관찰 노드가 구성원에게 묻는 시간
const PEER_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- 읽기

/// 이력 줄을 읽는다: 구성원이면 이 노드에서, 관찰 노드면 구성원(리더 먼저)에게.
async fn read(state: &AppState, req: HistoryRequest) -> Result<Vec<HistoryRecord>, ApiError> {
    if state.cluster.mode() == NodeMode::Member {
        return Ok(match req {
            HistoryRequest::Resource { kind, id } => state.cluster.history_of(kind, id)?,
            HistoryRequest::Timeline { before, limit } => state
                .cluster
                .history_timeline(before, limit.min(MAX_HISTORY_READ))?,
        });
    }
    let peers = state
        .peers
        .as_ref()
        .ok_or_else(|| unavailable("this node has no node-to-node connection"))?;
    let members = state.cluster.members();
    let mut order: Vec<(NodeId, String)> = Vec::new();
    if let Some(leader) = members.leader
        && let Some(addr) = members.voters.get(&leader)
    {
        order.push((leader, addr.clone()));
    }
    for (id, addr) in members.voters.iter().chain(&members.learners) {
        if !order.iter().any(|(o, _)| o == id) {
            order.push((*id, addr.clone()));
        }
    }
    let mut last = String::from("no Raft member to read from");
    for (id, addr) in order {
        match peers
            .read_history(id, &addr, req.clone(), PEER_TIMEOUT)
            .await
        {
            Ok(Some(records)) => return Ok(records),
            Ok(None) => last = format!("node {} does not serve history yet", id.0),
            Err(s) => last = s.message().to_string(),
        }
    }
    Err(unavailable(&last))
}

fn unavailable(why: &str) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "history_unavailable",
        format!("the configuration history lives on Raft members; cannot read it: {why}"),
    )
}

/// 리소스 하나의 줄(오래된 것부터)
async fn records_of(
    state: &AppState,
    kind: Kind,
    id: Uuid,
) -> Result<Vec<HistoryRecord>, ApiError> {
    read(state, HistoryRequest::Resource { kind, id }).await
}

/// 전체 이력(오래된 것부터 — 시점 되돌리기). 보존 한도 안이라 크기가 묶여 있다.
async fn all_records(state: &AppState) -> Result<Vec<HistoryRecord>, ApiError> {
    let mut out = Vec::new();
    let mut before = None;
    loop {
        let page = read(
            state,
            HistoryRequest::Timeline {
                before,
                limit: MAX_HISTORY_READ,
            },
        )
        .await?;
        // 빈 쪽까지 읽는다: 구성원이 바이트 예산으로 쪽을 줄일 수 있다(`fit_reply`).
        let Some(last) = page.last() else { break };
        before = Some(last.revision);
        out.extend(page);
    }
    out.reverse();
    Ok(out)
}

/// 이력 줄을 이 주체가 지금 읽을 수 있는지(§13, 목록·watch와 같은 규칙): 그 줄의 spec(그때의 폴더)과 지금 객체(있으면)를 둘
/// 다 읽을 수 있어야 한다 — 읽을 수 없는 폴더에서 옮겨 온 객체의 그때 줄은 보이지 않는다(리뷰 1).
struct Access<'a> {
    state: &'a AppState,
    principal: &'a Principal,
    owners: Owners,
}

impl<'a> Access<'a> {
    fn new(state: &'a AppState, principal: &'a Principal) -> Result<Self, ApiError> {
        Ok(Self {
            state,
            principal,
            owners: Owners::load(&state.cluster)?,
        })
    }

    fn can_read(&self, rec: &HistoryRecord) -> bool {
        self.live_readable(rec.kind, rec.id).unwrap_or(true) && self.row_readable(rec)
    }

    /// 지금 객체를 읽을 수 있는지(없으면 None)
    fn live_readable(&self, kind: Kind, id: Uuid) -> Option<bool> {
        match self.state.cluster.get_raw(kind, id) {
            Ok(Some(live)) => Some(self.readable(kind, &live)),
            _ => None,
        }
    }

    /// 그 줄의 spec(그때의 폴더·레이블)으로 읽을 수 있는지
    fn row_readable(&self, rec: &HistoryRecord) -> bool {
        let raw = RawResource {
            id: rec.id,
            name: rec.name.clone(),
            revision: rec.revision,
            labels: rec.labels.clone(),
            spec: rec.spec.clone(),
            status: Value::Null,
        };
        self.readable(rec.kind, &raw)
    }

    fn readable(&self, kind: Kind, raw: &RawResource) -> bool {
        match self.owners.of_event(kind, raw) {
            Some(owned) => self
                .state
                .authz
                .effective(self.principal, &owned)
                .is_ok_and(|set| set.contains(owned.read)),
            None => self
                .state
                .authorize(self.principal, Action::Read, kind)
                .is_ok(),
        }
    }

    /// 리소스 하나를 볼 수 있는지(거르기 전에 — 있는데 읽을 수 없는 것과 없는 것이 같은 404): 지금 객체, 지운 것은 가장
    /// 새 줄
    fn resource_readable(&self, kind: Kind, id: Uuid, newest: Option<&HistoryRecord>) -> bool {
        self.live_readable(kind, id)
            .or_else(|| newest.map(|r| self.row_readable(r)))
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------- 보기 꼴

/// 타임라인의 줄 하나(spec 본문 없음 — 가볍다)
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HistoryEntry {
    pub revision: u64,
    pub kind: Kind,
    pub id: Uuid,
    pub name: String,
    pub op: HistoryOp,
    /// 유닉스 밀리초(리더 시계)
    pub at_ms: u64,
    pub actor: Actor,
    pub source: ChangeSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reason: Option<String>,
    /// 감사 줄 id(`GET /audit`의 `id`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub request: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub apply_set: Option<Uuid>,
    /// 되돌리기면 되돌린 줄의 revision
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub restored_from: Option<u64>,
    /// 앞 줄과 견줘 바뀐 칸(최대 20개 — 레이블은 `labels.<열쇠>`). 만들기·지우기는 비어 있다.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
}

/// 이력 줄 목록(새것부터)
#[derive(Debug, Serialize, ToSchema)]
pub struct HistoryList {
    pub items: Vec<HistoryEntry>,
    /// 더 있을 수 있다: 다음 쪽은 `before=<이 값>`
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub next_before: Option<u64>,
}

/// 줄 하나의 전체와 비교
#[derive(Debug, Serialize, ToSchema)]
pub struct HistoryDetail {
    pub record: HistoryRecord,
    /// 견준 것(`prev`·`live`·revision)
    pub compare: String,
    /// 견준 줄의 revision(`live`는 지금 객체의 revision, 앞 줄이 없으면 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub against: Option<u64>,
    /// 견준 것 → 이 줄의 바뀜(비밀은 `$secret` 표시만)
    pub diff: Vec<Change>,
}

/// 바뀐 경로(spec 칸 + 레이블)
fn changed(prev: Option<&HistoryRecord>, rec: &HistoryRecord) -> Vec<String> {
    let Some(prev) = prev else {
        return Vec::new();
    };
    if rec.op != HistoryOp::Update {
        return Vec::new();
    }
    let mut out: Vec<String> = diff(&prev.spec, &rec.spec)
        .into_iter()
        .map(|c| c.path)
        .collect();
    let keys: BTreeSet<&String> = prev.labels.keys().chain(rec.labels.keys()).collect();
    for k in keys {
        if prev.labels.get(k) != rec.labels.get(k) {
            out.push(format!("labels.{k}"));
        }
    }
    if prev.name != rec.name {
        out.insert(0, "name".into());
    }
    out.truncate(MAX_CHANGED);
    out
}

fn entry(rec: &HistoryRecord, changed: Vec<String>) -> HistoryEntry {
    HistoryEntry {
        revision: rec.revision,
        kind: rec.kind,
        id: rec.id,
        name: rec.name.clone(),
        op: rec.op,
        at_ms: rec.at_ms,
        actor: rec.actor.clone(),
        source: rec.source,
        reason: rec.reason.clone(),
        request: rec.request,
        apply_set: rec.apply_set,
        restored_from: rec.restored_from,
        changed,
    }
}

/// 종류 이름(`vm`·`Vm`·`floating-ip`)
fn parse_any_kind(s: &str) -> Result<Kind, ApiError> {
    Kind::parse(s)
        .or_else(|| {
            ManifestKind::ALL
                .into_iter()
                .find(|k| k.as_str() == s)
                .map(ManifestKind::kind)
        })
        .filter(|k| k.history_tracked())
        .ok_or_else(|| ApiError::bad_request(format!("{s:?} is not a kind with a history")))
}

/// 리소스(id 또는 이름 — 지운 것은 id 또는 이력의 마지막 이름)
async fn resolve_key(state: &AppState, kind: Kind, key: &str) -> Result<Uuid, ApiError> {
    if let Ok(id) = key.parse::<Uuid>() {
        return Ok(id);
    }
    if let Some(live) = state.cluster.get_raw_by_name(kind, key)? {
        return Ok(live.id);
    }
    // 지운 리소스: 이름으로 가장 새 줄을 찾는다.
    let mut before = None;
    let mut scanned = 0;
    loop {
        let page = read(
            state,
            HistoryRequest::Timeline {
                before,
                limit: MAX_HISTORY_READ,
            },
        )
        .await?;
        let Some(last) = page.last() else { break };
        before = Some(last.revision);
        scanned += page.len();
        if let Some(r) = page.iter().find(|r| r.kind == kind && r.name == key) {
            return Ok(r.id);
        }
        if scanned >= MAX_SCAN {
            break;
        }
    }
    Err(not_found_key(kind, key))
}

/// 이 리소스가 없다 — 있지만 읽을 수 없는 것과 없는 것이 같은 본문이다(리뷰 2: 문구로도 폴더 너머의 있음을 알 수
/// 없게). 이력의 네 끝점이 모두 이것을 쓴다.
fn not_found_key(kind: Kind, key: &str) -> ApiError {
    ApiError::not_found(format!("{} {key:?} not found", kind.as_str()))
}

/// 줄 `revision`이 없거나 읽을 수 없다: 리소스를 볼 수 있으면 "그 줄이 없다", 아니면 [`not_found_key`]
fn no_row(
    access: &Access,
    kind: Kind,
    key: &str,
    id: Uuid,
    records: &[HistoryRecord],
    revision: u64,
) -> ApiError {
    if access.resource_readable(kind, id, records.last()) {
        ApiError::not_found(format!(
            "{} {key:?} has no history row {revision}",
            kind.as_str()
        ))
    } else {
        not_found_key(kind, key)
    }
}

// ---------------------------------------------------------------- GET /history

#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct HistoryQuery {
    /// 종류(`vm`·`Vm`)
    pub kind: Option<String>,
    /// 리소스 id 또는 이름(`kind`와 함께)
    pub key: Option<String>,
    /// 이 폴더 경로와 그 아래(`/prod` — 모든 트리, `vm:/prod` — 그 트리만)
    pub folder: Option<String>,
    /// 바꾼 사용자 이름
    pub user: Option<String>,
    /// 출처(`web`·`cli`·`api`·`apply`·`terraform`·`task`·`system`)
    pub source: Option<String>,
    /// 적용 묶음 이름(그 묶음이 쓴 줄)
    pub apply_set: Option<String>,
    /// 이 시각부터(유닉스 밀리초 또는 RFC 3339)
    pub since: Option<String>,
    /// 이 시각까지
    pub until: Option<String>,
    /// 이 revision 앞에서(쪽 열쇠 — 앞 답의 `next_before`)
    pub before: Option<u64>,
    /// 줄 수(기본 100, 최대 1 000)
    pub limit: Option<usize>,
}

/// 걸러 낼 조건
struct Filter {
    kind: Option<Kind>,
    id: Option<Uuid>,
    folder: Option<(Option<FolderKind>, String)>,
    user: Option<String>,
    source: Option<ChangeSource>,
    apply_set: Option<Uuid>,
    since: Option<u64>,
    until: Option<u64>,
}

fn parse_source(s: &str) -> Result<ChangeSource, ApiError> {
    [
        ChangeSource::Api,
        ChangeSource::Web,
        ChangeSource::Cli,
        ChangeSource::Apply,
        ChangeSource::Terraform,
        ChangeSource::Task,
        ChangeSource::System,
    ]
    .into_iter()
    .find(|c| c.as_str() == s)
    .ok_or_else(|| ApiError::bad_request(format!("unknown source {s:?}")))
}

fn parse_folder(s: &str) -> Result<(Option<FolderKind>, String), ApiError> {
    let (tree, path) = match s.split_once(':') {
        Some((t, p)) => {
            let tree = match t {
                "vm" => FolderKind::Vm,
                "network" => FolderKind::Network,
                "storage" => FolderKind::Storage,
                _ => return Err(ApiError::bad_request(format!("unknown folder tree {t:?}"))),
            };
            (Some(tree), p)
        }
        None => (None, s),
    };
    orbivisor_types::manifest::folder_path(path)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok((tree, path.trim_end_matches('/').to_string()))
}

impl Filter {
    fn matches(&self, rec: &HistoryRecord, folders: &FolderPaths) -> bool {
        if self.kind.is_some_and(|k| k != rec.kind) || self.id.is_some_and(|i| i != rec.id) {
            return false;
        }
        if self
            .user
            .as_deref()
            .is_some_and(|u| rec.actor.user.as_deref() != Some(u))
        {
            return false;
        }
        if self.source.is_some_and(|s| s != rec.source)
            || self.apply_set.is_some_and(|s| rec.apply_set != Some(s))
            || self.since.is_some_and(|t| rec.at_ms < t)
            || self.until.is_some_and(|t| rec.at_ms > t)
        {
            return false;
        }
        match &self.folder {
            None => true,
            Some((tree, path)) => folders.within(rec, *tree, path),
        }
    }
}

/// 폴더 id → (경로, 트리)(지금 있는 폴더 + 이력의 지운 폴더)
struct FolderPaths(BTreeMap<Uuid, (String, Option<Uuid>, FolderKind)>);

impl FolderPaths {
    fn load(state: &AppState) -> Result<Self, ApiError> {
        Ok(Self(
            state
                .cluster
                .list::<FolderSpec>()?
                .1
                .into_iter()
                .map(|f| (f.id, (f.spec.name, f.spec.parent, f.spec.kind)))
                .collect(),
        ))
    }

    fn path(&self, id: Uuid) -> Option<(String, FolderKind)> {
        let mut parts = Vec::new();
        let mut next = Some(id);
        let mut tree = None;
        while let Some(f) = next {
            let (name, parent, kind) = self.0.get(&f)?;
            tree.get_or_insert(*kind);
            parts.push(name.clone());
            next = *parent;
            if parts.len() > orbivisor_types::folder::MAX_FOLDER_DEPTH {
                return None;
            }
        }
        parts.reverse();
        Some((format!("/{}", parts.join("/")), tree?))
    }

    /// 줄의 리소스(폴더는 자기)가 `path` 아래인지
    fn within(&self, rec: &HistoryRecord, tree: Option<FolderKind>, path: &str) -> bool {
        let at = if rec.kind == Kind::Folder {
            Some(rec.id)
        } else {
            rec.spec
                .get("folder")
                .and_then(Value::as_str)
                .and_then(|s| s.parse().ok())
        };
        let Some((p, t)) = at.and_then(|f| self.path(f)) else {
            return path.is_empty() || path == "/";
        };
        if tree.is_some_and(|want| want != t) {
            return false;
        }
        path.is_empty() || path == "/" || p == path || p.starts_with(&format!("{path}/"))
    }
}

/// 설정 이력 타임라인(새것부터). 지금 읽을 수 있는 리소스의 줄만(지운 리소스는 마지막 폴더로) — 거른 뒤 `limit`을
/// 채운다. 다음 쪽은 `before=next_before`. 이력은 기능 `config-history`를 켠 때부터 있다. 구성원이 닿지 않는 관찰
/// 노드면 503 `history_unavailable`.
#[utoipa::path(get, path = "/history", tag = "history",
    params(HistoryQuery),
    responses((status = 200, body = HistoryList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn timeline(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryList>, ApiError> {
    require_feature(&state.cluster, CONFIG_HISTORY)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_LIMIT}"
        )));
    }
    let kind = q.kind.as_deref().map(parse_any_kind).transpose()?;
    let id = match (&q.key, kind) {
        (Some(key), Some(kind)) => Some(resolve_key(&state, kind, key).await?),
        (Some(_), None) => return Err(ApiError::bad_request("key needs a kind")),
        _ => None,
    };
    let apply_set = match &q.apply_set {
        Some(name) => Some(
            state
                .cluster
                .get_by_name::<ApplySetSpec>(name)?
                .ok_or_else(|| ApiError::not_found(format!("apply set {name:?} not found")))?
                .id,
        ),
        None => None,
    };
    let filter = Filter {
        kind,
        id,
        folder: q.folder.as_deref().map(parse_folder).transpose()?,
        user: q.user.clone(),
        source: q.source.as_deref().map(parse_source).transpose()?,
        apply_set,
        since: q
            .since
            .as_deref()
            .map(|s| parse_time("since", s))
            .transpose()?,
        until: q
            .until
            .as_deref()
            .map(|s| parse_time("until", s))
            .transpose()?,
    };
    let access = Access::new(&state, &principal)?;
    let folders = FolderPaths::load(&state)?;
    // 리소스 하나면 그 줄만 읽는다.
    if let (Some(kind), Some(id)) = (kind, id) {
        let records = records_of(&state, kind, id).await?;
        // 거르기 전에 본다(거른 결과로 있는지 알 수 없게 — 리뷰 1).
        if !access.resource_readable(kind, id, records.last()) {
            return Err(not_found_key(kind, q.key.as_deref().unwrap_or_default()));
        }
        let mut items = Vec::new();
        for (i, rec) in records.iter().enumerate().rev() {
            if q.before.is_some_and(|b| rec.revision >= b) || !filter.matches(rec, &folders) {
                continue;
            }
            if !access.row_readable(rec) {
                continue;
            }
            if items.len() == limit {
                return Ok(Json(HistoryList {
                    next_before: items.last().map(|e: &HistoryEntry| e.revision),
                    items,
                }));
            }
            let prev = i.checked_sub(1).map(|p| &records[p]);
            items.push(entry(rec, changed(prev, rec)));
        }
        return Ok(Json(HistoryList {
            items,
            next_before: None,
        }));
    }
    let mut items = Vec::new();
    let mut before = q.before;
    let mut scanned = 0;
    let mut prevs: BTreeMap<(Kind, Uuid), Vec<HistoryRecord>> = BTreeMap::new();
    let mut readable: BTreeMap<(Kind, Uuid), Option<bool>> = BTreeMap::new();
    let mut more = false;
    'pages: loop {
        let page = read(
            &state,
            HistoryRequest::Timeline {
                before,
                limit: MAX_HISTORY_READ.min(limit * 4).max(200),
            },
        )
        .await?;
        let Some(last) = page.last() else { break };
        let last_rev = last.revision;
        for rec in &page {
            scanned += 1;
            if !filter.matches(rec, &folders) {
                continue;
            }
            let live_ok = *readable
                .entry((rec.kind, rec.id))
                .or_insert_with(|| access.live_readable(rec.kind, rec.id));
            if !live_ok.unwrap_or(true) || !access.row_readable(rec) {
                continue;
            }
            if items.len() == limit {
                more = true;
                break 'pages;
            }
            // 앞 줄(바뀐 칸) — 바꾸기 줄만 그 리소스의 줄을 읽는다.
            let prev = if rec.op == HistoryOp::Update {
                let list = match prevs.get(&(rec.kind, rec.id)) {
                    Some(l) => l,
                    None => {
                        let l = records_of(&state, rec.kind, rec.id).await?;
                        prevs.entry((rec.kind, rec.id)).or_insert(l)
                    }
                };
                list.iter()
                    .rev()
                    .find(|p| p.revision < rec.revision)
                    .cloned()
            } else {
                None
            };
            items.push(entry(rec, changed(prev.as_ref(), rec)));
        }
        // 빈 쪽까지 읽는다(구성원이 바이트 예산으로 쪽을 줄일 수 있다).
        before = Some(last_rev);
        if scanned >= MAX_SCAN {
            more = true;
            break;
        }
    }
    let next_before = if more {
        items.last().map(|e| e.revision).or(before)
    } else {
        None
    };
    Ok(Json(HistoryList { items, next_before }))
}

/// 리소스 하나의 이력(새것부터 — 지운 리소스는 id 또는 마지막 이름으로). 읽을 수 없으면 404.
#[utoipa::path(get, path = "/history/{kind}/{key}", tag = "history",
    params(("kind" = String, Path, description = "kind (`vm`, `Vm`)"), ("key" = String, Path, description = "id or name")),
    responses((status = 200, body = HistoryList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn resource_history(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((kind, key)): Path<(String, String)>,
) -> Result<Json<HistoryList>, ApiError> {
    timeline(
        State(state),
        principal,
        Query(HistoryQuery {
            kind: Some(kind),
            key: Some(key),
            limit: Some(MAX_LIMIT),
            ..HistoryQuery::default()
        }),
    )
    .await
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CompareQuery {
    /// `prev`(앞 줄 — 기본)·`live`(지금 객체)·revision
    pub compare: Option<String>,
}

/// 비밀 표시를 하나로(지금 값과 견줄 때 — 이력에는 바뀜/같음만 있어 지금 평문과 견줄 수 없다)
fn neutral_secrets(v: &mut Value) {
    match v {
        Value::Object(m) if m.contains_key(SECRET_MARK) && m.len() == 1 => {
            *v = serde_json::json!({ SECRET_MARK: "set" });
        }
        Value::Object(m) => m.values_mut().for_each(neutral_secrets),
        Value::Array(a) => a.iter_mut().for_each(neutral_secrets),
        _ => {}
    }
}

/// 줄 하나의 전체 spec(비밀은 `$secret` 표시)과 비교: `prev`(기본) — 앞 줄 → 이 줄, `live` — 이 줄 → 지금 객체,
/// revision — 그 줄 → 이 줄. 읽을 수 없으면 404.
#[utoipa::path(get, path = "/history/{kind}/{key}/{revision}", tag = "history",
    params(("kind" = String, Path, description = "kind"), ("key" = String, Path, description = "id or name"),
        ("revision" = u64, Path, description = "the history row"), CompareQuery),
    responses((status = 200, body = HistoryDetail), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn history_row(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path((kind, key, revision)): Path<(String, String, u64)>,
    Query(q): Query<CompareQuery>,
) -> Result<Json<HistoryDetail>, ApiError> {
    require_feature(&state.cluster, CONFIG_HISTORY)?;
    let kind = parse_any_kind(&kind)?;
    let id = resolve_key(&state, kind, &key).await?;
    let records = records_of(&state, kind, id).await?;
    let access = Access::new(&state, &principal)?;
    let missing = || no_row(&access, kind, &key, id, &records, revision);
    let i = records
        .iter()
        .position(|r| r.revision == revision)
        .ok_or_else(missing)?;
    let rec = &records[i];
    if !access.can_read(rec) {
        return Err(missing());
    }
    let compare = q.compare.unwrap_or_else(|| "prev".into());
    let (against, diff) = match compare.as_str() {
        // 읽을 수 없는 앞 줄(그때 다른 폴더)은 없는 것처럼
        "prev" => match i
            .checked_sub(1)
            .map(|p| &records[p])
            .filter(|p| access.row_readable(p))
        {
            Some(prev) => (Some(prev.revision), diff(&prev.spec, &rec.spec)),
            None => (None, diff(&Value::Object(Default::default()), &rec.spec)),
        },
        "live" => match state.cluster.get_raw(kind, id)? {
            Some(live) => {
                let mut now = orbivisor_types::history::history_mask(kind, None, &live.spec);
                let mut then = rec.spec.clone();
                neutral_secrets(&mut now);
                neutral_secrets(&mut then);
                (Some(live.revision), diff(&then, &now))
            }
            None => (None, diff(&rec.spec, &Value::Object(Default::default()))),
        },
        other => {
            let r: u64 = other
                .parse()
                .map_err(|_| ApiError::bad_request("compare must be prev, live or a revision"))?;
            let base = records
                .iter()
                .find(|x| x.revision == r && access.row_readable(x))
                .ok_or_else(|| ApiError::not_found(format!("no history row {r}")))?;
            (Some(r), diff(&base.spec, &rec.spec))
        }
    };
    Ok(Json(HistoryDetail {
        record: rec.clone(),
        compare,
        against,
        diff,
    }))
}

// ---------------------------------------------------------------- 되돌리기

/// `POST /history/{kind}/{key}/restore`의 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryRestoreRequest {
    /// 되돌릴 이력 줄(그 줄의 spec으로 — 지우기 줄이면 지우기 직전 spec)
    pub revision: u64,
    /// 계획만(200 — 그 계획 id를 `POST /apply {plan}`으로 적용할 수 있다)
    #[serde(default)]
    pub dry_run: bool,
    /// 사유(머리 `Orbivisor-Change-Reason` 대신)
    #[serde(default)]
    #[schema(nullable = false)]
    pub reason: Option<String>,
    /// 지운 리소스의 이름이 그새 쓰이면 이 이름으로 살린다(새 id)
    #[serde(default)]
    #[schema(nullable = false)]
    pub name: Option<String>,
    /// Terraform이 관리하는 객체도 되돌린다
    #[serde(default)]
    pub force_conflicts: bool,
}

/// 되돌릴 매니페스트를 만들 때의 문제
enum Unrestorable {
    /// 단계 막힘(계획에 보인다)
    Blocked(Problem),
}

/// 이력 줄의 spec을 매니페스트로: 비밀 표시는 지금 값(살아 있으면 — 비밀은 되돌리지 않음) 또는 뺌(지운 것), 폴더는
/// 경로, 전원(`state`)은 되돌리지 않는다. 경고(비밀·이름)를 함께 돌려준다.
fn restore_doc(
    world: &World,
    kind: ManifestKind,
    rec: &HistoryRecord,
    live: Option<&Live>,
    name: Option<&str>,
) -> Result<(Manifest, Vec<Problem>), Unrestorable> {
    let mut warnings = Vec::new();
    let mut spec = rec.spec.clone();
    let mut secrets = false;
    fill_secrets(&mut spec, live.map(|l| &l.spec), &mut secrets);
    if secrets {
        warnings.push(Problem::warning(
            "secret_not_restored",
            match live {
                Some(_) => "secrets are not restored; the current secret values stay",
                None => "secrets are not restored; set them again after restoring",
            },
            None,
        ));
    }
    let fake = Live {
        kind,
        id: rec.id,
        name: match (live, name) {
            (Some(l), _) => l.name.clone(),
            (None, Some(n)) => n.to_string(),
            (None, None) => rec.name.clone(),
        },
        revision: live.map_or(0, |l| l.revision),
        labels: rec.labels.clone(),
        spec,
    };
    if let Some(l) = live
        && l.name != rec.name
    {
        warnings.push(Problem::warning(
            "name_not_restored",
            format!(
                "it was named {:?} then; the name stays {:?} (rename it by hand)",
                rec.name, l.name
            ),
            None,
        ));
    }
    if let Some(f) = fake.folder()
        && world.folder_path(f).is_none()
    {
        return Err(Unrestorable::Blocked(Problem::error(
            "missing_reference",
            "its folder was deleted; restore the folder first",
            Some("metadata.folder".into()),
        )));
    }
    let mut m = world.manifest_of(&fake);
    m.state = None;
    if live.is_none() {
        // 지운 것: 서버가 다는 레이블 말고 그때의 레이블
        m.metadata.labels.retain(|k, _| k != APPLY_SET_LABEL);
    }
    Ok((m, warnings))
}

/// 비밀 표시(`{"$secret": …}`)를 지금 값으로 바꾸거나(없으면) 칸을 뺀다.
fn fill_secrets(v: &mut Value, live: Option<&Value>, found: &mut bool) {
    match v {
        Value::Object(m) => {
            // 갈래(`kind`)가 지금과 다르다: 그 갈래의 비밀은 지금 값에 없다(리뷰 2)
            let switched = live.is_some_and(|l| {
                l.is_object() && m.get("kind").is_some() && l.get("kind") != m.get("kind")
            });
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                let child = m.get_mut(&k).expect("key");
                let marked = child
                    .as_object()
                    .is_some_and(|o| o.len() == 1 && o.contains_key(SECRET_MARK));
                let now = live.and_then(|l| l.get(&k));
                if marked {
                    *found = true;
                    match now {
                        Some(n) if !n.is_null() => *child = n.clone(),
                        // 참조로 둔다: 채울 지금 값이 없어 그 단계만 `secret_missing`으로 막힌다(빼면 꼭 있어야 할
                        // 칸이 없어 계획 전체가 400이다).
                        _ if switched => {
                            *child = serde_json::to_value(SecretValue::Ref {
                                secret_ref: String::new(),
                            })
                            .expect("secret ref");
                        }
                        _ => {
                            m.remove(&k);
                        }
                    }
                } else {
                    fill_secrets(child, now, found);
                }
            }
        }
        Value::Array(a) => {
            for (i, item) in a.iter_mut().enumerate() {
                fill_secrets(item, live.and_then(|l| l.get(i)), found);
            }
        }
        _ => {}
    }
}

/// 지운 데이터 종류 살리기 막힘(§12.3): 볼륨·백업 저장소는 막고, VM은 디스크 볼륨이 모두 살아 있어야 한다.
fn data_block(
    state: &AppState,
    kind: ManifestKind,
    rec: &HistoryRecord,
) -> Result<Option<Problem>, ApiError> {
    Ok(match kind {
        ManifestKind::Volume => Some(Problem::error(
            "data_lost",
            "a deleted volume's data is gone; restore it from a backup",
            None,
        )),
        ManifestKind::BackupStore => Some(Problem::error(
            "data_lost",
            "a deleted backup store must be added again (its repository is set up separately)",
            None,
        )),
        ManifestKind::Vm => {
            let mut gone = Vec::new();
            for d in rec
                .spec
                .get("disks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(v) = d.get("volume").and_then(Value::as_str)
                    && state.cluster.get_by_name::<VolumeSpec>(v)?.is_none()
                {
                    gone.push(v.to_string());
                }
            }
            (!gone.is_empty()).then(|| {
                Problem::error(
                    "data_lost",
                    format!(
                        "its disk volume(s) {} were deleted; restore them from a backup first",
                        gone.join(", ")
                    ),
                    None,
                )
            })
        }
        _ => None,
    })
}

/// 계획 앞에 막힌 단계 하나를 더한다(계획을 세우지 못한 리소스).
fn push_blocked(
    plan: &mut StoredPlan,
    kind: ManifestKind,
    name: &str,
    folder: Option<String>,
    problems: Vec<Problem>,
) {
    let n = plan.view.steps.len() + 1;
    plan.view.steps.push(StepView {
        n,
        kind,
        name: name.to_string(),
        folder,
        action: StepAction::Blocked,
        diff: Vec::new(),
        problems,
        restart: false,
        after: Vec::new(),
    });
    plan.steps.push(plan::Step {
        kind,
        key: name.to_string(),
        name: name.to_string(),
        action: StepAction::Blocked,
        seen: None,
        seen_config: 0,
        doc: None,
        declared_spec: Value::Null,
        declared: Value::Null,
        labels: BTreeMap::new(),
        fields: Vec::new(),
        spec_changed: false,
        labels_changed: false,
        reuse_id: None,
    });
    plan.view.summary = summarize(&plan.view.steps);
}

/// 단계 문제 앞에 경고를 더한다.
fn warn(plan: &mut StoredPlan, kind: ManifestKind, name: &str, warnings: Vec<Problem>) {
    if let Some(v) = plan
        .view
        .steps
        .iter_mut()
        .find(|v| v.kind == kind && v.name == name)
    {
        v.problems.extend(warnings);
    }
}

/// 계획을 남기고 적용하거나(202) 계획을 보인다(200).
async fn finish(
    state: &Arc<AppState>,
    plan: StoredPlan,
    dry_run: bool,
    audit: &AuditNote,
    detail: String,
) -> Result<Response, ApiError> {
    let stored = state.plans.keep(plan);
    if dry_run {
        return Ok(Json::<PlanView>(stored.view.clone()).into_response());
    }
    let started: ApplyStarted = start(state, &stored).await?;
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(started)).into_response())
}

fn meta_for(reason: Option<&str>, restored_from: u64) -> ChangeMeta {
    let mut meta = ChangeContext::current().unwrap_or_default();
    if let Some(r) = reason.and_then(clean_reason) {
        meta.reason = Some(r);
    }
    meta.restored_from = Some(restored_from);
    meta
}

/// 리소스 하나를 이력 줄 `revision`의 spec으로 되돌린다(선언형 계획 — 그 리소스만, 묶음 없음). 지운 리소스면 그 직전
/// spec으로 **같은 id**로 살린다(이름이 그새 쓰이면 409 `name_taken` — `name`으로 새 이름·새 id). 볼륨·백업 저장소는
/// 살릴 수 없고 VM은 디스크 볼륨이 모두 있어야 한다(막힘 `data_lost`). 비밀은 되돌리지 않는다(경고). 바꿀 수 없는 칸이
/// 다르면 막힘. 단계마다 그 종류의 지금 쓰기 권한이 든다. 답은 `POST /apply`와 같다(`dry_run` 200 계획, 202 Task,
/// 막힘 409 `plan_blocked`).
#[utoipa::path(post, path = "/history/{kind}/{key}/restore", tag = "history",
    params(("kind" = String, Path, description = "manifest kind"), ("key" = String, Path, description = "id or name")),
    request_body = HistoryRestoreRequest,
    responses(
        (status = 200, body = PlanView, description = "the plan (dry run)"),
        (status = 202, body = ApplyStarted),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn restore_one(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path((kind, key)): Path<(String, String)>,
    Json(req): Json<HistoryRestoreRequest>,
) -> Result<Response, ApiError> {
    require_feature(&state.cluster, CONFIG_HISTORY)?;
    require_feature(&state.cluster, DECLARATIVE)?;
    state
        .authz
        .require_anywhere(&principal, Privilege::SystemRead)?;
    let mk = parse_kind(&kind)?;
    let id = resolve_key(&state, mk.kind(), &key).await?;
    let records = records_of(&state, mk.kind(), id).await?;
    let access = Access::new(&state, &principal)?;
    let missing = || no_row(&access, mk.kind(), &key, id, &records, req.revision);
    let rec = records
        .iter()
        .find(|r| r.revision == req.revision)
        .cloned()
        .ok_or_else(missing)?;
    if !access.can_read(&rec) {
        return Err(missing());
    }
    let world = World::load(&state.cluster)?;
    let live = world.by_id(mk, id).cloned();
    let mut reuse = BTreeMap::new();
    let mut blocked = None;
    let name = if live.is_none() {
        if let Some(n) = &req.name {
            crate::resources::check_name(n)?;
        }
        let want = req.name.clone().unwrap_or_else(|| rec.name.clone());
        if state.cluster.get_raw_by_name(mk.kind(), &want)?.is_some() {
            return Err(ApiError::conflict(
                "name_taken",
                format!(
                    "{} {want:?} is taken; give another name (name) to restore it as a new object",
                    mk.as_str()
                ),
            ));
        }
        blocked = data_block(&state, mk, &rec)?;
        Some(want)
    } else {
        None
    };
    let (doc, warnings) = match restore_doc(&world, mk, &rec, live.as_ref(), name.as_deref()) {
        Ok(d) => d,
        Err(Unrestorable::Blocked(p)) => {
            blocked = Some(p);
            (Manifest::new(mk, rec.name.clone(), Value::Null), Vec::new())
        }
    };
    let doc_key =
        crate::declarative::kinds::key_of_doc(&doc).unwrap_or_else(|_| doc.metadata.name.clone());
    let a = adapter(mk).ok_or_else(|| ApiError::internal("no adapter"))?;
    if live.is_none() && req.name.is_none() && a.keeps_id_on_restore() {
        reuse.insert((mk, doc_key), id);
    }
    let meta = meta_for(req.reason.as_deref(), rec.revision);
    let mut made = match blocked {
        Some(p) => {
            let mut empty = Box::pin(plan::make_plan(
                &state,
                &principal,
                &plan::Request::default(),
                meta,
            ))
            .await?;
            push_blocked(
                &mut empty,
                mk,
                &doc.metadata.name,
                doc.metadata.folder.clone(),
                vec![p],
            );
            empty
        }
        None => {
            let request = plan::Request {
                documents: vec![doc.clone()],
                force_conflicts: req.force_conflicts,
                reuse_ids: reuse,
                replace: true,
                ..plan::Request::default()
            };
            Box::pin(plan::make_plan(&state, &principal, &request, meta)).await?
        }
    };
    warn(&mut made, mk, &doc.metadata.name, warnings);
    let detail = format!(
        "history restore {} {} -> r{}",
        mk.kind().as_str(),
        doc.metadata.name,
        rec.revision
    );
    finish(&state, made, req.dry_run, &audit, detail).await
}

/// 시점(`at`)
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum RestorePoint {
    /// 이 revision을 적용한 뒤
    Revision(u64),
    /// 이 시각(유닉스 밀리초 또는 RFC 3339)
    Time(String),
}

/// 시점 되돌리기의 범위(하나 이상 — 모두 맞아야 한다)
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreScope {
    /// 종류(`Vm`·`vm`)
    #[serde(default)]
    pub kinds: Vec<String>,
    /// 이 폴더 경로와 그 아래(`/prod` — 모든 트리, `vm:/prod`)
    #[serde(default)]
    #[schema(nullable = false)]
    pub folder: Option<String>,
    /// 이 적용 묶음의 것
    #[serde(default)]
    #[schema(nullable = false)]
    pub apply_set: Option<String>,
    /// `종류/이름`(`Vm/web01`)
    #[serde(default)]
    pub keys: Vec<String>,
}

/// `POST /history/restore`의 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PointRestoreRequest {
    pub at: RestorePoint,
    pub scope: RestoreScope,
    /// 그 뒤 지운 것을 살린다(같은 id)
    #[serde(default = "yes")]
    pub recreate_deleted: bool,
    /// 그 뒤 만든 것을 지운다
    #[serde(default)]
    pub delete_created: bool,
    /// 그 뒤 만든 VM·볼륨·백업 저장소도 지운다(`delete_created`와 함께)
    #[serde(default)]
    pub allow_data_loss: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    #[schema(nullable = false)]
    pub reason: Option<String>,
    #[serde(default)]
    pub force_conflicts: bool,
}

/// 시점 판단(줄이 그 시점 앞인지)
type AtFn = Box<dyn Fn(&HistoryRecord) -> bool + Send + Sync>;

fn yes() -> bool {
    true
}

/// 리소스 하나의 시점 판단
enum AtPoint<'a> {
    /// 그때 spec으로(지금 있으면 바꾸기, 지웠으면 살리기)
    Restore(&'a HistoryRecord),
    /// 그 뒤 만든 것
    CreatedAfter,
    /// 그때를 알 수 없다(보존 한도로 앞 줄이 없다)
    Expired,
    /// 할 일 없음
    Nothing,
}

fn at_point<'a>(
    records: &'a [HistoryRecord],
    at: &dyn Fn(&HistoryRecord) -> bool,
    live: bool,
) -> AtPoint<'a> {
    let before = records.iter().rev().find(|r| at(r));
    match before {
        Some(r) if r.op != HistoryOp::Delete => {
            let newest = records.last().expect("records");
            if live && newest.revision == r.revision {
                AtPoint::Nothing
            } else {
                AtPoint::Restore(r)
            }
        }
        Some(_) => {
            if live {
                AtPoint::CreatedAfter
            } else {
                AtPoint::Nothing
            }
        }
        None => {
            if !live {
                return AtPoint::Nothing;
            }
            match records.first() {
                Some(first) if first.op == HistoryOp::Create => AtPoint::CreatedAfter,
                Some(_) => AtPoint::Expired,
                None => AtPoint::Nothing,
            }
        }
    }
}

/// 여러 리소스를 한 시점으로 되돌린다(§12.4): 범위 안 리소스마다 그 시점의 spec(그 전 마지막 줄)으로 문서를 만들어 한
/// 계획을 세운다 — 바뀐 것은 바꾸기, 그 뒤 지운 것은 살리기(`recreate_deleted` — §12.3 규칙), 그 뒤 만든 것은
/// `delete_created`면 지우기(VM·볼륨·백업 저장소는 `allow_data_loss`도). 보존 한도로 그때를 알 수 없는 리소스는
/// `blocked: history_expired`. 의존 차례·권한·미리 확인·저장한 계획이 `POST /apply`와 같다.
#[utoipa::path(post, path = "/history/restore", tag = "history",
    request_body = PointRestoreRequest,
    responses(
        (status = 200, body = PlanView, description = "the plan (dry run)"),
        (status = 202, body = ApplyStarted),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn restore_point(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<PointRestoreRequest>,
) -> Result<Response, ApiError> {
    require_feature(&state.cluster, CONFIG_HISTORY)?;
    require_feature(&state.cluster, DECLARATIVE)?;
    state
        .authz
        .require_anywhere(&principal, Privilege::SystemRead)?;
    let scope = &req.scope;
    if scope.kinds.is_empty()
        && scope.folder.is_none()
        && scope.apply_set.is_none()
        && scope.keys.is_empty()
    {
        return Err(ApiError::bad_request(
            "give a scope (kinds, folder, apply_set or keys); restoring the whole cluster at once is not supported",
        ));
    }
    let kinds: Vec<ManifestKind> = scope
        .kinds
        .iter()
        .map(|k| parse_kind(k))
        .collect::<Result<_, _>>()?;
    let keys: Vec<(ManifestKind, String)> = scope
        .keys
        .iter()
        .map(|k| {
            let (kind, name) = k
                .split_once('/')
                .ok_or_else(|| ApiError::bad_request(format!("key {k:?} must be KIND/NAME")))?;
            Ok((parse_kind(kind)?, name.to_string()))
        })
        .collect::<Result<_, ApiError>>()?;
    let folder = scope.folder.as_deref().map(parse_folder).transpose()?;
    let set_ids: Option<(String, BTreeSet<Uuid>)> = match &scope.apply_set {
        Some(name) => {
            let set = state
                .cluster
                .get_by_name::<ApplySetSpec>(name)?
                .ok_or_else(|| ApiError::not_found(format!("apply set {name:?} not found")))?;
            Some((
                set.name.clone(),
                set.status.objects.iter().map(|o| o.id).collect(),
            ))
        }
        None => None,
    };
    let (at_desc, at): (String, AtFn) = match &req.at {
        RestorePoint::Revision(r) => {
            let r = *r;
            (
                format!("r{r}"),
                Box::new(move |x: &HistoryRecord| x.revision <= r),
            )
        }
        RestorePoint::Time(t) => {
            let ms = parse_time("at", t)?;
            (t.clone(), Box::new(move |x: &HistoryRecord| x.at_ms <= ms))
        }
    };

    let all = all_records(&state).await?;
    // 되돌린 시점의 revision(시각이면 그때까지의 마지막 줄 — 이력 줄의 `restored_from`)
    let point_revision = all
        .iter()
        .filter(|r| at(r))
        .map(|r| r.revision)
        .max()
        .unwrap_or(0);
    let mut by: BTreeMap<(Kind, Uuid), Vec<HistoryRecord>> = BTreeMap::new();
    for r in all {
        by.entry((r.kind, r.id)).or_default().push(r);
    }
    let access = Access::new(&state, &principal)?;
    let mut world = World::load(&state.cluster)?;
    let mut folders = FolderPaths::load(&state)?;
    // 그때 있던 지운 폴더도 경로를 풀 수 있게(같은 계획에서 살린다)
    for ((kind, id), records) in &by {
        if *kind != Kind::Folder || folders.0.contains_key(id) {
            continue;
        }
        if let AtPoint::Restore(r) = at_point(records, &*at, false)
            && let Ok(f) = serde_json::from_value::<FolderSpec>(r.spec.clone())
        {
            folders.0.insert(*id, (f.name.clone(), f.parent, f.kind));
            world.add_folder(*id, f.name, f.parent, f.kind);
        }
    }

    let mut documents = Vec::new();
    let mut reuse = BTreeMap::new();
    let mut delete = Vec::new();
    let mut blocked: Vec<(ManifestKind, String, Option<String>, Problem)> = Vec::new();
    let mut warnings: Vec<(ManifestKind, String, Vec<Problem>)> = Vec::new();
    for ((kind, id), records) in &by {
        let Some(mk) = ManifestKind::from_kind(*kind).filter(|k| adapter(*k).is_some()) else {
            continue;
        };
        let live = world.by_id(mk, *id).cloned();
        let point = at_point(records, &*at, live.is_some());
        let newest = records.last().expect("records");
        // 범위: 그때의 줄(또는 지금 값)로 본다.
        let probe = match &point {
            AtPoint::Restore(r) => (*r).clone(),
            _ => newest.clone(),
        };
        let name_now = live.as_ref().map_or(probe.name.clone(), |l| l.name.clone());
        if !kinds.is_empty() && !kinds.contains(&mk) {
            continue;
        }
        if !keys.is_empty()
            && !keys
                .iter()
                .any(|(k, n)| *k == mk && (*n == name_now || *n == probe.name))
        {
            continue;
        }
        if let Some((tree, path)) = &folder
            && !folders.within(&probe, *tree, path)
            && !folders.within(newest, *tree, path)
        {
            continue;
        }
        if let Some((set_name, ids)) = &set_ids
            && !ids.contains(id)
            && probe.labels.get(APPLY_SET_LABEL) != Some(set_name)
        {
            continue;
        }
        if !access.can_read(&probe) {
            continue;
        }
        match point {
            AtPoint::Nothing => {}
            AtPoint::Expired => blocked.push((
                mk,
                name_now,
                None,
                Problem::error(
                    "history_expired",
                    "the history no longer reaches that point (retention); restore it by hand",
                    None,
                ),
            )),
            AtPoint::CreatedAfter => {
                if req.delete_created {
                    delete.push((mk, *id));
                }
            }
            AtPoint::Restore(r) => {
                if live.is_none() {
                    if !req.recreate_deleted {
                        continue;
                    }
                    if state.cluster.get_raw_by_name(*kind, &r.name)?.is_some() {
                        blocked.push((
                            mk,
                            r.name.clone(),
                            None,
                            Problem::error(
                                "name_taken",
                                format!("{} {:?} is taken by another object", mk.as_str(), r.name),
                                None,
                            ),
                        ));
                        continue;
                    }
                    if let Some(p) = data_block(&state, mk, r)? {
                        blocked.push((mk, r.name.clone(), None, p));
                        continue;
                    }
                }
                match restore_doc(&world, mk, r, live.as_ref(), None) {
                    Ok((doc, w)) => {
                        if live.is_none() && adapter(mk).is_some_and(|a| a.keeps_id_on_restore()) {
                            let key = crate::declarative::kinds::key_of_doc(&doc)
                                .unwrap_or_else(|_| doc.metadata.name.clone());
                            reuse.insert((mk, key), *id);
                        }
                        if !w.is_empty() {
                            warnings.push((mk, doc.metadata.name.clone(), w));
                        }
                        documents.push(doc);
                    }
                    Err(Unrestorable::Blocked(p)) => blocked.push((mk, r.name.clone(), None, p)),
                }
            }
        }
    }
    let count = documents.len() + delete.len();
    let request = plan::Request {
        documents,
        prune_data: req.allow_data_loss,
        force_conflicts: req.force_conflicts,
        delete,
        reuse_ids: reuse,
        replace: true,
        ..plan::Request::default()
    };
    let meta = meta_for(req.reason.as_deref(), point_revision);
    let mut made = Box::pin(plan::make_plan(&state, &principal, &request, meta)).await?;
    for (mk, name, w) in warnings {
        warn(&mut made, mk, &name, w);
    }
    for (mk, name, folder, p) in blocked {
        push_blocked(&mut made, mk, &name, folder, vec![p]);
    }
    let scope_desc = scope
        .folder
        .clone()
        .or_else(|| scope.apply_set.as_ref().map(|s| format!("set {s}")))
        .unwrap_or_else(|| "selected objects".into());
    let detail = format!("history restore {scope_desc} at {at_desc} ({count} objects)");
    finish(&state, made, req.dry_run, &audit, detail).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(revision: u64, op: HistoryOp, spec: Value) -> HistoryRecord {
        HistoryRecord::new(
            &ChangeMeta::default(),
            revision,
            Kind::Vm,
            Uuid::nil(),
            "web".into(),
            op,
            BTreeMap::new(),
            spec,
        )
    }

    #[test]
    fn points_in_time_are_judged_from_the_rows_before() {
        let rows = vec![
            rec(10, HistoryOp::Create, json!({ "a": 1 })),
            rec(20, HistoryOp::Update, json!({ "a": 2 })),
        ];
        let at = |r: u64| move |x: &HistoryRecord| x.revision <= r;
        assert!(matches!(at_point(&rows, &at(15), true), AtPoint::Restore(r) if r.revision == 10));
        assert!(matches!(at_point(&rows, &at(25), true), AtPoint::Nothing));
        assert!(matches!(
            at_point(&rows, &at(5), true),
            AtPoint::CreatedAfter
        ));
        // 지운 뒤: 그때 있었으면 살리기, 그 뒤 만든 것은 할 일 없음
        let gone = vec![
            rec(10, HistoryOp::Create, json!({})),
            rec(30, HistoryOp::Delete, json!({})),
        ];
        assert!(matches!(
            at_point(&gone, &at(15), false),
            AtPoint::Restore(_)
        ));
        assert!(matches!(at_point(&gone, &at(5), false), AtPoint::Nothing));
        // 보존 한도로 만들기 줄이 없다.
        let trimmed = vec![rec(20, HistoryOp::Update, json!({}))];
        assert!(matches!(
            at_point(&trimmed, &at(15), true),
            AtPoint::Expired
        ));
    }

    #[test]
    fn secret_marks_take_the_live_value_or_go() {
        let mut spec = json!({ "auth": { "password": { "$secret": "changed" }, "user": "u" }, "list": [{ "k": { "$secret": "set" } }] });
        let live = json!({ "auth": { "password": "now", "user": "x" }, "list": [{ "k": "live" }] });
        let mut found = false;
        fill_secrets(&mut spec, Some(&live), &mut found);
        assert!(found);
        assert_eq!(
            spec,
            json!({ "auth": { "password": "now", "user": "u" }, "list": [{ "k": "live" }] })
        );
        let mut spec = json!({ "auth": { "password": { "$secret": "same" } } });
        let mut found = false;
        fill_secrets(&mut spec, None, &mut found);
        assert_eq!(spec, json!({ "auth": {} }));
    }

    #[test]
    fn changed_lists_spec_label_and_name_paths() {
        let a = rec(
            1,
            HistoryOp::Create,
            json!({ "cpu": { "cores": 1 }, "memory_mib": 512 }),
        );
        let mut b = rec(
            2,
            HistoryOp::Update,
            json!({ "cpu": { "cores": 2 }, "memory_mib": 512 }),
        );
        b.labels.insert("tier".into(), "web".into());
        b.name = "web2".into();
        assert_eq!(changed(Some(&a), &b), ["name", "cpu.cores", "labels.tier"]);
        assert!(changed(None, &b).is_empty());
        assert!(changed(Some(&b), &rec(3, HistoryOp::Delete, json!({}))).is_empty());
    }

    #[test]
    fn folders_filter_by_path_and_tree() {
        let prod = Uuid::now_v7();
        let web = Uuid::now_v7();
        let paths = FolderPaths(BTreeMap::from([
            (prod, ("prod".to_string(), None, FolderKind::Vm)),
            (web, ("web".to_string(), Some(prod), FolderKind::Vm)),
        ]));
        let r = rec(1, HistoryOp::Create, json!({ "folder": web.to_string() }));
        assert!(paths.within(&r, None, "/prod"));
        assert!(paths.within(&r, Some(FolderKind::Vm), "/prod/web"));
        assert!(!paths.within(&r, Some(FolderKind::Network), "/prod"));
        assert!(!paths.within(&r, None, "/pro"));
        assert!(!paths.within(&rec(1, HistoryOp::Create, json!({})), None, "/prod"));
    }
}
