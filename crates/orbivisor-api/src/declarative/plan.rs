//! 계획 세우기(계획 73 §8.2): 문서 검증 → 의존 차례(위상 정렬, 같은 층은 종류 차례) → 단계마다 "볼 수 있는 것만"으로
//! 참조 풀기 → 세 갈래 비교(선언한 칸만 + 묶음의 앞 선언에서 빠진 칸) → 권한(바꿀 것이 있을 때, 거절이면 그 단계의 나머지
//! 검사를 하지 않는다) → 쓰지 않는 미리 확인 → 가지치기. 아무것도 쓰지 않는다.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Instant;

use orbivisor_types::apply_set::{AppliedObject, ApplySet, ApplySetSpec};
use orbivisor_types::history::ChangeMeta;
use orbivisor_types::manifest::{
    APPLY_SET_LABEL, MANAGED_BY_LABEL, MAX_DOCUMENTS, Manifest, ManifestKind, PRUNE_ANNOTATION,
};
use orbivisor_types::placement::PlacementRuleSpec;
use orbivisor_types::storage::VolumeSpec;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::diff::{Change, diff, forget_dropped, overlay, top_fields};
use super::kinds::{
    Check, Live, Planned, Update, World, adapter, check_route, folder_key, key_of_doc, rank,
    readable, tree_of_doc, typed,
};
use super::{Manager, PlanView, Problem, StepAction, StepView, Summary};
use crate::auth::Principal;
use crate::error::ApiError;
use crate::resources::{CreateRequest, check_name};
use crate::state::AppState;

/// 계획을 세울 요청(문서는 풀어 둔 것)
#[derive(Default)]
pub(crate) struct Request {
    pub documents: Vec<Manifest>,
    pub apply_set: Option<String>,
    pub prune: bool,
    pub prune_data: bool,
    pub manager: Option<Manager>,
    /// 다른 관리자(Terraform·다른 적용 묶음)의 객체도 바꾼다(`--force-conflicts` — 관리를 넘겨받는다)
    pub force_conflicts: bool,
    /// 문서와 따로 지울 객체(시점 되돌리기의 "그 뒤 만든 것" — §12.4). 데이터 종류는 `prune_data`가 있어야 한다.
    pub delete: Vec<(ManifestKind, Uuid)>,
    /// 만들기 단계가 쓸 옛 id(지운 것 살리기 — §12.3, (종류, 문서 열쇠) → id)
    pub reuse_ids: BTreeMap<(ManifestKind, String), Uuid>,
    /// 되돌리기(§12): 문서가 spec 전체다 — 지금 값에 있고 문서에 없는 칸(그때 기본값이라 이력 줄에 빠진 칸)은 기본값으로
    /// 돌아간다. 비밀 칸은 문서에 없어도 지금 값 그대로다(비밀은 되돌리지 않는다).
    pub replace: bool,
}

/// 객체를 맡은 관리자(§8.2 7 — 객체 통째 단위)
#[derive(Debug, Clone, PartialEq, Eq)]
enum Owner {
    Terraform,
    Set(String),
}

fn owner_of(labels: &BTreeMap<String, String>) -> Option<Owner> {
    if labels.get(MANAGED_BY_LABEL).map(String::as_str) == Some("terraform") {
        return Some(Owner::Terraform);
    }
    labels.get(APPLY_SET_LABEL).cloned().map(Owner::Set)
}

/// 이 요청이 다른 관리자의 객체를 바꾸는지. 묶음 없는 적용(되돌리기 포함)은 묶음 객체를 바꿔도 묶음을 빼앗지 않아
/// 충돌이 아니다 — Terraform 객체만 막는다.
fn conflict(labels: &BTreeMap<String, String>, req: &Request) -> Option<Owner> {
    let owner = owner_of(labels)?;
    let me = match (req.manager, &req.apply_set) {
        (Some(Manager::Terraform), _) => Some(Owner::Terraform),
        (None, Some(set)) => Some(Owner::Set(set.clone())),
        (None, None) => None,
    };
    match (&owner, &me) {
        (o, Some(m)) if o == m => None,
        (Owner::Set(_), None) => None,
        _ => Some(owner),
    }
}

fn managed_elsewhere(kind: ManifestKind, name: &str, owner: &Owner) -> Problem {
    let by = match owner {
        Owner::Terraform => "Terraform".to_string(),
        Owner::Set(s) => format!("apply set {s:?}"),
    };
    Problem::error(
        "managed_elsewhere",
        format!(
            "{} {name:?} is managed by {by}; change it there, or take it over with --force-conflicts (force_conflicts)",
            kind.as_str()
        ),
        None,
    )
}

/// 계획의 단계 하나(적용이 쓰는 것까지)
#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub kind: ManifestKind,
    pub key: String,
    /// 리소스 이름(폴더는 화면 이름)
    pub name: String,
    pub action: StepAction,
    /// 계획 때 본 객체(id, revision)
    pub seen: Option<(Uuid, u64)>,
    /// 계획 때 본 객체의 설정 지문([`Live::config_fingerprint`] — 이름·레이블·spec). 상태만 쓴 것(revision은 오른다)은
    /// 계획을 낡게 하지 않는다(리뷰 1).
    pub seen_config: u64,
    /// 원하는 매니페스트(만들기는 선언, 바꾸기는 지금 값에 선언을 얹은 것)
    pub doc: Option<Manifest>,
    /// 선언한 spec 그대로(서버 칸을 지금 값에서 가져올 때 "선언했는지"를 본다)
    pub declared_spec: Value,
    /// 묶음에 남길 선언(§6 `AppliedObject.declared` — 비밀은 `$secret`)
    pub declared: Value,
    pub labels: BTreeMap<String, String>,
    /// 바뀐 맨 위 칸(저장 꼴)
    pub fields: Vec<String>,
    pub spec_changed: bool,
    pub labels_changed: bool,
    /// 만들기가 되살릴 옛 id(지운 것 살리기)
    pub reuse_id: Option<Uuid>,
}

/// 저장한 계획(받은 노드 메모리에 30분 — §8.3)
pub(crate) struct StoredPlan {
    pub id: Uuid,
    pub owner: Principal,
    pub hash: String,
    pub created: Instant,
    pub view: PlanView,
    pub steps: Vec<Step>,
    pub apply_set: Option<String>,
    pub manager: Option<Manager>,
    /// 계획을 세운 요청의 출처(주체·사유·감사 id) — 적용 Task가 이어 쓴다.
    pub meta: ChangeMeta,
}

impl StoredPlan {
    pub(crate) fn blocked(&self) -> bool {
        self.view.summary.blocked > 0
    }
}

/// 문서 묶음의 해시(저장한 계획을 같은 문서로 적용하는지)
pub(crate) fn hash(req: &Request) -> String {
    let body = json!({
        "documents": req.documents,
        "apply_set": req.apply_set,
        "prune": req.prune,
        "prune_data": req.prune_data,
        "manager": req.manager,
        "force_conflicts": req.force_conflicts,
        "delete": req.delete,
    });
    hex::encode(Sha256::digest(
        serde_json::to_vec(&body).expect("documents serialize"),
    ))
}

fn invalid(i: usize, e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(format!("documents[{i}]: {e}")).with_code("manifest_invalid")
}

/// 문서 묶음을 검사하고 (종류, 열쇠)를 정한다.
fn check_documents(docs: &[Manifest]) -> Result<Vec<String>, ApiError> {
    if docs.len() > MAX_DOCUMENTS {
        return Err(ApiError::bad_request(format!(
            "at most {MAX_DOCUMENTS} documents in one apply, not {}; split them",
            docs.len()
        ))
        .with_code("manifest_invalid"));
    }
    let mut keys = Vec::with_capacity(docs.len());
    let mut seen = BTreeSet::new();
    for (i, doc) in docs.iter().enumerate() {
        doc.validate().map_err(|e| invalid(i, e))?;
        if adapter(doc.kind).is_none() {
            return Err(ApiError::bad_request(format!(
                "documents[{i}]: {} documents are not supported yet",
                doc.kind.as_str()
            ))
            .with_code("manifest_unsupported"));
        }
        if doc.kind != ManifestKind::Folder {
            check_name(&doc.metadata.name).map_err(|e| invalid(i, e.message))?;
        } else {
            orbivisor_types::folder::validate_folder_name(&doc.metadata.name)
                .map_err(|e| invalid(i, e))?;
        }
        let tree = tree_of_doc(doc).map_err(|e| invalid(i, e))?;
        if tree.is_none() && doc.metadata.folder.is_some() {
            return Err(invalid(
                i,
                format!("{} documents have no metadata.folder", doc.kind.as_str()),
            ));
        }
        let key = key_of_doc(doc).map_err(|e| invalid(i, e))?;
        if !seen.insert((doc.kind, key.clone())) {
            return Err(ApiError::bad_request(format!(
                "documents[{i}]: {} {key:?} is declared twice",
                doc.kind.as_str()
            ))
            .with_code("manifest_duplicate"));
        }
        keys.push(key);
    }
    Ok(keys)
}

/// 문서가 든 폴더의 열쇠(뿌리면 None)
fn folder_ref(doc: &Manifest) -> Option<(ManifestKind, String)> {
    let tree = tree_of_doc(doc).ok().flatten()?;
    let path = doc.metadata.folder.as_deref().filter(|p| *p != "/")?;
    Some((ManifestKind::Folder, folder_key(tree, path)))
}

/// 문서의 참조(폴더 포함)
fn refs(doc: &Manifest) -> Vec<(ManifestKind, String)> {
    let a = adapter(doc.kind).expect("checked");
    let mut out = a.refs(&doc.spec);
    out.extend(folder_ref(doc));
    out
}

/// 위상 정렬: 참조가 먼저, 같은 층은 종류 차례·문서 차례. 고리는 400 `manifest_cycle`(고리를 보인다).
fn order(docs: &[Manifest], keys: &[String]) -> Result<Vec<usize>, ApiError> {
    let index: BTreeMap<(ManifestKind, &str), usize> = docs
        .iter()
        .zip(keys)
        .enumerate()
        .map(|(i, (d, k))| ((d.kind, k.as_str()), i))
        .collect();
    let deps: Vec<BTreeSet<usize>> = docs
        .iter()
        .map(|d| {
            refs(d)
                .iter()
                .filter_map(|(k, key)| index.get(&(*k, key.as_str())).copied())
                .collect()
        })
        .collect();
    let mut done = vec![false; docs.len()];
    let mut out = Vec::with_capacity(docs.len());
    while out.len() < docs.len() {
        let next = (0..docs.len())
            .filter(|&i| !done[i] && deps[i].iter().all(|&d| done[d]))
            .min_by_key(|&i| (rank(docs[i].kind), i));
        let Some(i) = next else {
            return Err(cycle(docs, keys, &deps, &done));
        };
        done[i] = true;
        out.push(i);
    }
    Ok(out)
}

fn cycle(docs: &[Manifest], keys: &[String], deps: &[BTreeSet<usize>], done: &[bool]) -> ApiError {
    // 남은 것에서 의존을 따라가면 고리에 든다.
    let start = (0..docs.len()).find(|&i| !done[i]).expect("a doc is left");
    let mut path = vec![start];
    let mut at = start;
    while let Some(&next) = deps[at].iter().find(|&&d| !done[d]) {
        if let Some(pos) = path.iter().position(|&p| p == next) {
            path = path[pos..].to_vec();
            path.push(next);
            break;
        }
        path.push(next);
        at = next;
    }
    let names: Vec<String> = path
        .iter()
        .map(|&i| format!("{} {}", docs[i].kind.as_str(), keys[i]))
        .collect();
    ApiError::bad_request(format!(
        "the documents refer to each other in a cycle: {}",
        names.join(" -> ")
    ))
    .with_code("manifest_cycle")
}

/// 원하는 레이블: 지금 레이블에 선언을 얹고, 앞 선언에서 빠진 것을 지우고, 묶음·관리자 레이블을 단다. Terraform은 객체의
/// 레이블을 통째로 맡는다(선언하지 않은 레이블은 뗀다 — 서버가 다는 `orbivisor.io/` 레이블만 남김): 공급자 상태가 앞 선언이라
/// 묶음의 세 갈래 기억이 없고, 설정에서 뺀 레이블이 서버에 남으면 다음 plan이 늘 다르다.
fn labels_of(
    live: Option<&Live>,
    doc: &Manifest,
    prev: Option<&Value>,
    set: Option<&str>,
    manager: Option<Manager>,
    replace: bool,
) -> BTreeMap<String, String> {
    let mut out = live.map(|l| l.labels.clone()).unwrap_or_default();
    if replace {
        // 되돌리기(§12): 레이블도 그 줄 그대로 — 서버가 다는 `orbivisor.io/` 레이블만 지금 값을 둔다.
        out.retain(|k, _| k.starts_with("orbivisor.io/"));
    }
    if let Some(prev) = prev
        .and_then(|p| p.pointer("/metadata/labels"))
        .and_then(Value::as_object)
    {
        for k in prev.keys() {
            if !doc.metadata.labels.contains_key(k) {
                out.remove(k);
            }
        }
    }
    out.extend(doc.metadata.labels.clone());
    match manager {
        Some(Manager::Terraform) => {
            out.retain(|k, _| {
                k.starts_with("orbivisor.io/") || doc.metadata.labels.contains_key(k)
            });
            // Terraform이 넘겨받으면 묶음 레이블을 뗀다(그 묶음의 가지치기가 지우지 않게).
            out.remove(APPLY_SET_LABEL);
            out.insert(MANAGED_BY_LABEL.into(), "terraform".into());
        }
        None => {
            if let Some(set) = set {
                out.insert(APPLY_SET_LABEL.into(), set.into());
                out.remove(MANAGED_BY_LABEL);
            }
        }
    }
    out
}

/// 묶음에 남길 선언: 매니페스트에서 판·종류·이름을 뺀 것. 비밀 칸의 평문은 `{"$secret": "set"}`(지문 없이), 보기가
/// 가리는 칸(webhook URL·헤더 값)은 보기처럼 줄인다.
pub(crate) fn declared_of(doc: &Manifest) -> Value {
    let mut spec = doc.spec.clone();
    if let Some(a) = adapter(doc.kind) {
        for path in a.secret_paths() {
            let pointer = format!("/{}", path.replace('.', "/"));
            if let Some(v) = spec.pointer_mut(&pointer)
                && v.is_string()
            {
                *v = json!({ "$secret": "set" });
            }
        }
        a.redact_declared(&mut spec);
    }
    let mut out = json!({ "spec": spec });
    let meta = serde_json::to_value(&doc.metadata).expect("metadata");
    let mut meta = meta;
    if let Some(m) = meta.as_object_mut() {
        m.remove("name");
    }
    out["metadata"] = meta;
    if let Some(state) = doc.state {
        out["state"] = serde_json::to_value(state).expect("state");
    }
    out
}

/// 비밀 칸의 값을 바뀜 표시로 가린다(§8.2 — "비밀은 diff에 `(changed)`만").
fn mask(kind: ManifestKind, changes: &mut [Change]) {
    let Some(a) = adapter(kind) else { return };
    for c in changes.iter_mut() {
        for secret in a.secret_paths() {
            if c.path == *secret {
                for v in [&mut c.old, &mut c.new].into_iter().flatten() {
                    *v = json!("(changed)");
                }
            } else if let Some(rest) = secret.strip_prefix(&format!("{}.", c.path)) {
                let pointer = format!("/{}", rest.replace('.', "/"));
                for v in [&mut c.old, &mut c.new].into_iter().flatten() {
                    if let Some(s) = v.pointer_mut(&pointer)
                        && !s.is_null()
                    {
                        *s = json!("(secret)");
                    }
                }
            }
        }
    }
}

/// 권한 거절(403·404)의 문제. 볼 수 없는 것의 이름·폴더를 새지 않게 처리기의 문구를 쓰지 않는다(404도 같은 문구).
fn forbidden(kind: ManifestKind, name: &str) -> Problem {
    Problem::error(
        "forbidden",
        format!("you may not change {} {name:?}", kind.as_str()),
        None,
    )
}

/// 문서 `i`의 단계 재료
struct Doc<'a> {
    doc: &'a Manifest,
    key: &'a str,
}

/// 계획을 세운다(쓰지 않는다).
pub(crate) async fn make_plan(
    state: &Arc<AppState>,
    principal: &Principal,
    req: &Request,
    meta: ChangeMeta,
) -> Result<StoredPlan, ApiError> {
    let keys = check_documents(&req.documents)?;
    let world = World::load(&state.cluster)?;
    let set: Option<ApplySet> = match &req.apply_set {
        Some(name) => {
            check_name(name)?;
            state.cluster.get_by_name::<ApplySetSpec>(name)?
        }
        None => None,
    };
    let previous = set
        .as_ref()
        .map(|s| s.status.objects.clone())
        .unwrap_or_default();

    let mut planned = Planned::default();
    for (doc, key) in req.documents.iter().zip(&keys) {
        planned.docs.insert((doc.kind, key.clone()), doc.clone());
    }
    // VM 미리 확인이 얹는 배치 규칙(규칙은 VM 뒤에 만들지만 확인은 함께 본다 — 계획 71)
    for (doc, key) in req.documents.iter().zip(&keys) {
        if doc.kind == ManifestKind::PlacementRule
            && !world.objects.contains_key(&(doc.kind, key.clone()))
            && let Ok(spec) = typed::<PlacementRuleSpec>(&doc.spec)
        {
            planned.rules.push(CreateRequest {
                name: doc.metadata.name.clone(),
                labels: doc.metadata.labels.clone(),
                spec,
            });
        }
    }

    let order = order(&req.documents, &keys)?;
    let mut steps: Vec<Step> = Vec::new();
    let mut views: Vec<StepView> = Vec::new();
    // 문서 번호 → 단계 번호(의존 표시)
    let mut step_of: BTreeMap<(ManifestKind, String), usize> = BTreeMap::new();
    for &i in &order {
        let d = Doc {
            doc: &req.documents[i],
            key: &keys[i],
        };
        let n = steps.len() + 1;
        let after: Vec<usize> = refs(d.doc)
            .iter()
            .filter_map(|r| step_of.get(r).copied())
            .collect();
        let (step, mut view) = Box::pin(plan_doc(
            state,
            principal,
            &world,
            &mut planned,
            &d,
            n,
            req,
            &previous,
        ))
        .await?;
        view.after = after;
        step_of.insert((d.doc.kind, d.key.to_string()), n);
        steps.push(step);
        views.push(view);
    }

    // 가지치기(§8.2 6): 묶음이 기억하는데 문서에 없는 것을 역순으로 지운다.
    if let (Some(set), true) = (&set, req.prune) {
        let mut gone: Vec<(&AppliedObject, &Live)> = Vec::new();
        for obj in &set.status.objects {
            let Some(kind) = ManifestKind::from_kind(obj.kind) else {
                continue;
            };
            if steps
                .iter()
                .any(|s| s.kind == kind && s.seen.is_some_and(|(id, _)| id == obj.id))
            {
                continue;
            }
            let Some(live) = world.by_id(kind, obj.id) else {
                continue;
            };
            if !adapter(kind).is_some_and(|a| a.deletable()) {
                continue;
            }
            if live.labels.get(APPLY_SET_LABEL) != Some(&set.name)
                || !readable(state, principal, live)
            {
                continue;
            }
            let keep = obj
                .declared
                .pointer(&format!(
                    "/metadata/annotations/{}",
                    PRUNE_ANNOTATION.replace('/', "~1")
                ))
                .and_then(Value::as_str)
                == Some("false");
            if keep {
                continue;
            }
            gone.push((obj, live));
        }
        gone.sort_by_key(|(_, l)| {
            let depth = world
                .folder_path(l.id)
                .map_or(0, |p| p.matches('/').count());
            (std::cmp::Reverse(rank(l.kind)), std::cmp::Reverse(depth))
        });
        for (obj, live) in gone {
            let protect = set.spec.protect_data && !req.prune_data;
            let (step, view) = delete_step(
                state,
                principal,
                &world,
                live,
                obj.declared.clone(),
                protect,
                steps.len() + 1,
            );
            steps.push(step);
            views.push(view);
        }
    }

    // 따로 지울 것(시점 되돌리기): 같은 역순
    let mut extra: Vec<&Live> = req
        .delete
        .iter()
        .filter_map(|(k, id)| world.by_id(*k, *id))
        .filter(|l| adapter(l.kind).is_some_and(|a| a.deletable()))
        .filter(|l| {
            !steps
                .iter()
                .any(|s| s.seen.is_some_and(|(id, _)| id == l.id))
        })
        .collect();
    extra.sort_by_key(|l| {
        let depth = world
            .folder_path(l.id)
            .map_or(0, |p| p.matches('/').count());
        (std::cmp::Reverse(rank(l.kind)), std::cmp::Reverse(depth))
    });
    for live in extra {
        if !readable(state, principal, live) {
            continue;
        }
        let (step, view) = delete_step(
            state,
            principal,
            &world,
            live,
            Value::Null,
            !req.prune_data,
            steps.len() + 1,
        );
        steps.push(step);
        views.push(view);
    }
    for step in steps.iter_mut().filter(|s| s.action == StepAction::Create) {
        step.reuse_id = req.reuse_ids.get(&(step.kind, step.key.clone())).copied();
    }

    let summary = summarize(&views);
    let id = Uuid::now_v7();
    Ok(StoredPlan {
        id,
        owner: principal.clone(),
        hash: hash(req),
        created: Instant::now(),
        view: PlanView {
            plan: id,
            base_revision: world.revision,
            apply_set: req.apply_set.clone(),
            steps: views,
            summary,
        },
        steps,
        apply_set: req.apply_set.clone(),
        manager: req.manager,
        meta,
    })
}

/// 단계 수 세기
pub(crate) fn summarize(views: &[StepView]) -> Summary {
    let mut summary = Summary::default();
    for v in views {
        match v.action {
            StepAction::Create => summary.create += 1,
            StepAction::Update => summary.update += 1,
            StepAction::Delete => summary.delete += 1,
            StepAction::Unchanged => summary.unchanged += 1,
            StepAction::Blocked => summary.blocked += 1,
        }
    }
    summary
}

/// 지우기 단계 하나(권한이 없으면 `forbidden`, 데이터 종류는 `protect`면 `protected`로 막힘)
fn delete_step(
    state: &AppState,
    principal: &Principal,
    world: &World,
    live: &Live,
    declared: Value,
    protect: bool,
    n: usize,
) -> (Step, StepView) {
    let a = adapter(live.kind).expect("adapter");
    let mut problems = Vec::new();
    if check_route(state, principal, a.delete_route(), Some(live.id)).is_err() {
        problems.push(forbidden(live.kind, &live.name));
    } else if live.kind.holds_data() && protect {
        problems.push(Problem::error(
            "protected",
            format!(
                "{} {:?} holds data; prune it with --prune-data (prune_data)",
                live.kind.as_str(),
                live.name
            ),
            None,
        ));
    }
    let action = if problems.iter().any(Problem::is_error) {
        StepAction::Blocked
    } else {
        StepAction::Delete
    };
    let view = StepView {
        n,
        kind: live.kind,
        name: live.name.clone(),
        folder: live.folder().and_then(|f| world.folder_path(f)),
        action,
        diff: Vec::new(),
        problems,
        restart: false,
        after: Vec::new(),
    };
    let step = Step {
        kind: live.kind,
        key: world.key_of_live(live),
        name: live.name.clone(),
        action,
        seen: Some((live.id, live.revision)),
        seen_config: live.config_fingerprint(),
        doc: None,
        declared_spec: Value::Null,
        declared,
        labels: live.labels.clone(),
        fields: Vec::new(),
        spec_changed: false,
        labels_changed: false,
        reuse_id: None,
    };
    (step, view)
}

/// 지금 객체에 선언을 얹은 비교(§8.2 4 — 선언한 칸만 + 앞 선언에서 빠진 칸). 답은 (얹은 매니페스트 spec, 원하는 저장
/// spec, 바뀐 칸). 참조를 풀지 못하면 안쪽 `Err`(문구)다. 계획과 드리프트 검사가 함께 쓴다.
#[allow(clippy::type_complexity)]
pub(crate) fn compare(
    state: &AppState,
    world: &World,
    live: &Live,
    doc: &Manifest,
    folder: Option<Uuid>,
    prev_spec: Option<&Value>,
) -> Result<Result<(Value, Value, Vec<Change>), String>, ApiError> {
    let a = adapter(live.kind).expect("adapter");
    let mut merged = a.to_manifest(&live.spec, world);
    overlay(&mut merged, &doc.spec);
    if let Some(prev_spec) = prev_spec {
        forget_dropped(&mut merged, prev_spec, &doc.spec);
    }
    let mut store = match a.to_store(&merged, &live.name, folder, doc.state, world) {
        Ok(s) => s,
        Err(e) => return Ok(Err(e)),
    };
    a.keep_server_fields(state, &doc.spec, &mut store, &live.spec);
    let desired = a
        .normalize(&store)
        .map_err(|e| invalid_doc(live.kind, &doc.metadata.name, e))?;
    let current = a
        .normalize(&live.spec)
        .unwrap_or_else(|_| live.spec.clone());
    let changes = diff(&current, &desired);
    Ok(Ok((merged, desired, changes)))
}

/// 되돌리기의 "앞 선언": 지금 값의 매니페스트 spec 전체에서 비밀 칸을 뺀 것. 문서에 없는 끝 칸은 [`forget_dropped`]가
/// 지워 기본값으로 돌아가고, 비밀 칸은 지금 값 그대로다 — 단 그 칸의 갈래(부모의 `kind` — DNS 공급자·알림 대상 종류)가
/// 문서와 같을 때만이다. 갈래가 바뀌었으면 지금 비밀도 지워진다(리뷰 2 — 다른 갈래의 칸이 남아 문서 전체가 400이 되지
/// 않게. 문서 갈래의 비밀은 되돌리기가 참조로 두어 그 단계만 `secret_missing`으로 막힌다).
fn whole_spec(a: &dyn super::kinds::Adapter, live: &Live, world: &World, doc: &Value) -> Value {
    let mut spec = a.to_manifest(&live.spec, world);
    for path in a.secret_paths() {
        let path: Vec<String> = path.split('.').map(str::to_string).collect();
        let parent = &path[..path.len() - 1];
        let tag = |v: &Value| super::diff::get_path(v, parent).map(|p| p.get("kind").cloned());
        if tag(&spec) == tag(doc) {
            super::diff::remove_path(&mut spec, &path);
        }
    }
    spec
}

/// 문서 하나의 단계
#[allow(clippy::too_many_arguments)]
async fn plan_doc(
    state: &Arc<AppState>,
    principal: &Principal,
    world: &World,
    planned: &mut Planned,
    d: &Doc<'_>,
    n: usize,
    req: &Request,
    previous: &[AppliedObject],
) -> Result<(Step, StepView), ApiError> {
    let doc = d.doc;
    let kind = doc.kind;
    let a = adapter(kind).expect("checked");
    let live = world.objects.get(&(kind, d.key.to_string()));
    let name = doc.metadata.name.clone();
    let mut problems: Vec<Problem> = Vec::new();
    let mut step = Step {
        kind,
        key: d.key.to_string(),
        name: name.clone(),
        action: StepAction::Blocked,
        seen: live.map(|l| (l.id, l.revision)),
        seen_config: live.map_or(0, |l| l.config_fingerprint()),
        doc: None,
        declared_spec: doc.spec.clone(),
        declared: declared_of(doc),
        labels: BTreeMap::new(),
        fields: Vec::new(),
        spec_changed: false,
        labels_changed: false,
        reuse_id: None,
    };
    let mut view = StepView {
        n,
        kind,
        name: name.clone(),
        folder: doc.metadata.folder.clone(),
        action: StepAction::Blocked,
        diff: Vec::new(),
        problems: Vec::new(),
        restart: false,
        after: Vec::new(),
    };
    let blocked = |mut view: StepView, step: Step, problems: Vec<Problem>| {
        view.problems = problems;
        (step, view)
    };

    // 있지만 볼 수 없는 객체: 바꿀 수 없다(그 안을 보이지 않는다).
    if let Some(live) = live
        && !readable(state, principal, live)
    {
        problems.push(forbidden(kind, &name));
        return Ok(blocked(view, step, problems));
    }

    // 폴더: 있는(볼 수 있는) 폴더 또는 같은 계획이 만들 폴더
    let tree = tree_of_doc(doc).map_err(ApiError::bad_request)?;
    let (folder, folder_for_check) = match (tree, doc.metadata.folder.as_deref()) {
        (Some(tree), Some(path)) if path != "/" => {
            let key = folder_key(tree, path);
            let found = world
                .objects
                .get(&(ManifestKind::Folder, key.clone()))
                .filter(|l| readable(state, principal, l))
                .map(|l| l.id);
            match found {
                Some(id) => (Some(id), Some(id)),
                None if planned.docs.contains_key(&(ManifestKind::Folder, key)) => {
                    let near = world.nearest_folder(tree, path).filter(|id| {
                        world
                            .objects
                            .values()
                            .find(|l| l.kind == ManifestKind::Folder && l.id == *id)
                            .is_some_and(|l| readable(state, principal, l))
                    });
                    (None, near)
                }
                None => {
                    problems.push(Problem::error(
                        "missing_reference",
                        format!("folder {path:?} not found"),
                        Some("metadata.folder".into()),
                    ));
                    return Ok(blocked(view, step, problems));
                }
            }
        }
        _ => (None, None),
    };

    // 참조: 같은 계획 안 또는 볼 수 있는 지금 것(볼 수 없는 것은 없는 것과 같은 문구)
    for (rk, rkey) in a.refs(&doc.spec) {
        if planned.docs.contains_key(&(rk, rkey.clone())) {
            continue;
        }
        let ok = world
            .objects
            .get(&(rk, rkey.clone()))
            .is_some_and(|l| readable(state, principal, l));
        if !ok {
            problems.push(Problem::error(
                "missing_reference",
                format!("{} {rkey:?} not found", rk.as_str()),
                None,
            ));
        }
    }
    if !problems.is_empty() {
        return Ok(blocked(view, step, problems));
    }

    let set = req.apply_set.as_deref();
    match live {
        None => {
            let declared = if doc.spec.is_null() {
                json!({})
            } else {
                doc.spec.clone()
            };
            let mut store = match a.to_store(&declared, &name, folder_for_check, doc.state, world) {
                Ok(s) => s,
                Err(e) => {
                    problems.push(Problem::error("missing_reference", e, None));
                    return Ok(blocked(view, step, problems));
                }
            };
            if let Err(e) = a.create_defaults(state, world, planned, &mut store) {
                problems.push(super::kinds::problem(None, e));
                return Ok(blocked(view, step, problems));
            }
            let normalized = a
                .normalize(&store)
                .map_err(|e| invalid_doc(kind, &name, e))?;
            step.labels = labels_of(None, doc, None, set, req.manager, false);
            if a.may_create(state, principal, &store, folder_for_check)
                .is_err()
            {
                problems.push(forbidden(kind, &name));
                return Ok(blocked(view, step, problems));
            }
            problems.extend(
                a.precheck(Check {
                    state,
                    principal,
                    name: &name,
                    labels: &step.labels,
                    desired: &store,
                    live: None,
                    planned,
                })
                .await?,
            );
            let mut changes = diff(&json!({}), &normalized);
            mask(kind, &mut changes);
            view.diff = changes;
            if kind == ManifestKind::Volume
                && let Ok(spec) = typed::<VolumeSpec>(&store)
            {
                planned.volumes.push(CreateRequest {
                    name: name.clone(),
                    labels: step.labels.clone(),
                    spec,
                });
            }
            let mut m = doc.clone();
            m.spec = declared;
            step.doc = Some(m);
            step.spec_changed = true;
            step.action = StepAction::Create;
        }
        Some(live) => {
            let prev = previous
                .iter()
                .find(|o| o.id == live.id && o.kind == kind.kind())
                .map(|o| &o.declared);
            let whole = req.replace.then(|| whole_spec(a, live, world, &doc.spec));
            let (merged, desired, mut changes) = match compare(
                state,
                world,
                live,
                doc,
                folder,
                whole.as_ref().or(prev.and_then(|p| p.get("spec"))),
            )? {
                Ok(c) => c,
                Err(e) => {
                    problems.push(Problem::error("missing_reference", e, None));
                    return Ok(blocked(view, step, problems));
                }
            };
            let fields = top_fields(&changes);
            step.labels = labels_of(Some(live), doc, prev, set, req.manager, req.replace);
            step.labels_changed = step.labels != live.labels;
            step.spec_changed = !changes.is_empty();
            step.fields = fields.clone();
            let mut m = doc.clone();
            m.spec = merged;
            step.doc = Some(m);
            if changes.is_empty() && !step.labels_changed {
                step.action = StepAction::Unchanged;
                view.action = StepAction::Unchanged;
                return Ok((step, view));
            }
            if check_route(state, principal, a.update_route(), Some(live.id))
                .and_then(|()| a.may_update_extra(state, principal, live))
                .is_err()
            {
                problems.push(forbidden(kind, &name));
                return Ok(blocked(view, step, problems));
            }
            if !req.force_conflicts
                && let Some(owner) = conflict(&live.labels, req)
            {
                problems.push(managed_elsewhere(kind, &name, &owner));
            }
            // 폴더는 `POST /folders/{id}/members`로만 옮긴다(옮기기 권한을 따로 본다).
            let field = if kind == ManifestKind::Folder {
                "parent"
            } else {
                "folder"
            };
            if fields.iter().any(|f| f == field) {
                problems.push(Problem::error(
                    "immutable",
                    format!(
                        "{} {name:?} is in another folder; move it with `orbivisorctl folder move` (POST /folders/{{id}}/members) first",
                        kind.as_str()
                    ),
                    Some("metadata.folder".into()),
                ));
            }
            if let Update::Fields(allowed) = a.updatable() {
                for f in fields
                    .iter()
                    .filter(|f| *f != field && !allowed.contains(&f.as_str()))
                {
                    problems.push(Problem::error(
                        "immutable",
                        format!(
                            "{} {name:?}: {f} cannot change; delete and recreate it by hand",
                            kind.as_str()
                        ),
                        Some(f.clone()),
                    ));
                }
            }
            if problems.is_empty() && step.spec_changed {
                problems.extend(
                    a.precheck(Check {
                        state,
                        principal,
                        name: &name,
                        labels: &step.labels,
                        desired: &desired,
                        live: Some(live),
                        planned,
                    })
                    .await?,
                );
            }
            if kind == ManifestKind::Vm {
                view.restart = super::kinds::vm_needs_restart(&state.cluster, live.id, &desired);
            }
            mask(kind, &mut changes);
            view.diff = changes;
            step.action = StepAction::Update;
        }
    }
    if problems.iter().any(Problem::is_error) {
        step.action = StepAction::Blocked;
    }
    view.action = step.action;
    view.problems = problems;
    Ok((step, view))
}

fn invalid_doc(kind: ManifestKind, name: &str, e: String) -> ApiError {
    ApiError::bad_request(format!("{} {name:?}: spec: {e}", kind.as_str()))
        .with_code("manifest_invalid")
}

/// 저장한 계획이 아직 맞는지(§8.3): 단계가 본 객체의 revision이 그대로이고 만들 것이 아직 없다. 아니면 412 `plan_stale`.
pub(crate) fn still_valid(state: &AppState, plan: &StoredPlan) -> Result<(), ApiError> {
    let world = World::load(&state.cluster)?;
    for step in &plan.steps {
        let ok = match step.seen {
            None => !world.objects.contains_key(&(step.kind, step.key.clone())),
            Some((id, _)) => world
                .by_id(step.kind, id)
                .is_some_and(|l| l.config_fingerprint() == step.seen_config),
        };
        if !ok {
            return Err(ApiError::new(
                axum::http::StatusCode::PRECONDITION_FAILED,
                "plan_stale",
                format!(
                    "{} {:?} changed after the plan was made; plan again",
                    step.kind.as_str(),
                    step.name
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 고리 문구는 고리만 보인다(계획 73의 지금 종류로는 고리를 만들 수 없다 — 이름·경로 참조가 한쪽으로만 간다. 스테이지
    /// 3의 종류가 쓴다).
    #[test]
    fn a_cycle_names_its_members() {
        let docs: Vec<Manifest> = ["a", "b", "c"]
            .iter()
            .map(|n| Manifest::new(ManifestKind::Vm, *n, json!({})))
            .collect();
        let keys: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        // a → b → c → b
        let deps = vec![
            BTreeSet::from([1]),
            BTreeSet::from([2]),
            BTreeSet::from([1]),
        ];
        let e = cycle(&docs, &keys, &deps, &[false; 3]);
        assert_eq!(e.code, "manifest_cycle");
        assert!(e.message.ends_with("Vm b -> Vm c -> Vm b"), "{}", e.message);
    }

    #[test]
    fn documents_are_ordered_by_reference_then_kind() {
        let mut vm = Manifest::new(
            ManifestKind::Vm,
            "web",
            json!({ "disks": [{ "volume": "root" }], "nics": [{ "network": "user" }] }),
        );
        vm.metadata.folder = Some("/prod".into());
        let docs = vec![
            vm,
            Manifest::new(ManifestKind::Volume, "root", json!({ "pool": "local" })),
            Manifest::new(ManifestKind::SecurityGroup, "sg", json!({})),
            Manifest::new(ManifestKind::Folder, "prod", json!({ "kind": "vm" })),
        ];
        let keys = check_documents(&docs).unwrap();
        assert_eq!(keys[3], "vm:/prod");
        assert_eq!(order(&docs, &keys).unwrap(), [3, 2, 1, 0]);
    }

    fn request(manager: Option<Manager>, set: Option<&str>) -> Request {
        Request {
            apply_set: set.map(str::to_string),
            manager,
            ..Request::default()
        }
    }

    #[test]
    fn terraform_and_sets_do_not_take_each_others_objects() {
        let tf = BTreeMap::from([(MANAGED_BY_LABEL.to_string(), "terraform".to_string())]);
        let web = BTreeMap::from([(APPLY_SET_LABEL.to_string(), "web".to_string())]);
        let none = BTreeMap::new();
        let terraform = request(Some(Manager::Terraform), None);
        assert_eq!(conflict(&tf, &terraform), None);
        assert_eq!(conflict(&tf, &request(None, None)), Some(Owner::Terraform));
        assert_eq!(
            conflict(&tf, &request(None, Some("web"))),
            Some(Owner::Terraform)
        );
        assert_eq!(conflict(&web, &terraform), Some(Owner::Set("web".into())));
        assert_eq!(
            conflict(&web, &request(None, Some("db"))),
            Some(Owner::Set("web".into()))
        );
        assert_eq!(conflict(&web, &request(None, Some("web"))), None);
        // 묶음 없는 적용·되돌리기는 묶음 객체를 빼앗지 않는다.
        assert_eq!(conflict(&web, &request(None, None)), None);
        assert_eq!(conflict(&none, &terraform), None);
        // 넘겨받으면 다른 관리자의 레이블을 뗀다.
        let doc = Manifest::new(ManifestKind::SecurityGroup, "sg", json!({}));
        let live = Live {
            kind: ManifestKind::SecurityGroup,
            id: Uuid::nil(),
            name: "sg".into(),
            revision: 1,
            labels: tf.clone(),
            spec: json!({}),
        };
        let labels = labels_of(Some(&live), &doc, None, Some("web"), None, false);
        assert_eq!(labels.get(APPLY_SET_LABEL).map(String::as_str), Some("web"));
        assert!(!labels.contains_key(MANAGED_BY_LABEL));
        let live = Live {
            labels: web,
            ..live
        };
        let labels = labels_of(
            Some(&live),
            &doc,
            None,
            None,
            Some(Manager::Terraform),
            false,
        );
        assert!(!labels.contains_key(APPLY_SET_LABEL));
    }

    #[test]
    fn terraform_owns_all_labels_but_the_servers() {
        let mut doc = Manifest::new(ManifestKind::SecurityGroup, "sg", json!({}));
        doc.metadata.labels = BTreeMap::from([("tier".to_string(), "web".to_string())]);
        let live = Live {
            kind: ManifestKind::SecurityGroup,
            id: Uuid::nil(),
            name: "sg".into(),
            revision: 1,
            labels: BTreeMap::from([
                ("tier".to_string(), "db".to_string()),
                ("added-in-web".to_string(), "x".to_string()),
                ("orbivisor.io/other".to_string(), "keep".to_string()),
            ]),
            spec: json!({}),
        };
        let labels = labels_of(
            Some(&live),
            &doc,
            None,
            None,
            Some(Manager::Terraform),
            false,
        );
        assert_eq!(
            labels,
            BTreeMap::from([
                ("orbivisor.io/other".to_string(), "keep".to_string()),
                (MANAGED_BY_LABEL.to_string(), "terraform".to_string()),
                ("tier".to_string(), "web".to_string()),
            ])
        );
        // 다른 손님(묶음 없는 적용)은 선언하지 않은 레이블을 그대로 둔다.
        let labels = labels_of(Some(&live), &doc, None, None, None, false);
        assert!(labels.contains_key("added-in-web"));
        // 되돌리기는 그 줄의 레이블 그대로 — 뒤에 단 레이블은 떼고 서버 레이블은 둔다(배포 확인에서 찾음).
        let labels = labels_of(Some(&live), &doc, None, None, None, true);
        assert_eq!(
            labels,
            BTreeMap::from([
                ("orbivisor.io/other".to_string(), "keep".to_string()),
                ("tier".to_string(), "web".to_string()),
            ])
        );
    }

    #[test]
    fn declared_secrets_are_marked_not_kept() {
        let doc = Manifest::new(
            ManifestKind::Vm,
            "web",
            json!({ "cloud_init": { "password": "hunter2", "user": "rocky" } }),
        );
        let declared = declared_of(&doc);
        assert_eq!(
            declared["spec"]["cloud_init"]["password"],
            json!({ "$secret": "set" })
        );
        assert_eq!(declared["spec"]["cloud_init"]["user"], "rocky");
        assert!(!declared.to_string().contains("hunter2"));
        assert!(declared["metadata"].get("name").is_none());
    }

    /// 리뷰 1: webhook URL의 경로·헤더 값도 묶음에 남지 않는다.
    #[test]
    fn declared_webhook_url_and_headers_are_redacted() {
        let doc = Manifest::new(
            ManifestKind::NotificationTarget,
            "slack",
            json!({ "target": { "kind": "webhook",
                "url": "https://hooks.slack.com/services/T0/B0/SECRETPATH",
                "headers": { "Authorization": "Bearer SECRETHDR" }, "token": "TOKEN" } }),
        );
        let declared = declared_of(&doc);
        assert_eq!(
            declared["spec"]["target"],
            json!({ "kind": "webhook", "url": "https://hooks.slack.com/…",
                    "headers": { "Authorization": "" }, "token": { "$secret": "set" } })
        );
        assert!(
            !declared.to_string().contains("SECRET") && !declared.to_string().contains("TOKEN")
        );
    }
}
