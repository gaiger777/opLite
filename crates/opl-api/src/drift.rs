//! 적용 묶음 드리프트(계획 73 §12.5): 리더 `DriftChecker`가 클러스터가 바뀌면(10초 몰아 받기) 그리고 5분마다 묶음마다
//! 마지막 선언(`status.objects[].declared`)과 실제를 계획과 같은 비교("선언한 칸만")로 견주어 `status.drift`를 쓴다 —
//! 결과가 바뀔 때만 쓴다. 다른 칸마다 묶음이 적용한 뒤 그 칸을 처음 바꾼 이력 줄을 `by`에 단다(누가·언제·어디서·왜).
//! `POST /apply-sets/{key}/adopt`가 실제를 선언으로 받아들인다(묶음 status만 바꾼다).

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::apply_set::{
    AppliedObject, ApplySet, ApplySetSpec, ApplySetStatus, DriftItem, DriftReport, DriftState,
};
use opl_types::authz::Privilege;
use opl_types::history::{HistoryOp, HistoryRecord, HistoryRef, SECRET_MARK};
use opl_types::manifest::{APPLY_SET_LABEL, Manifest, ManifestKind, SecretValue, folder_path};
use opl_types::node::DECLARATIVE;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::cluster::{missing_feature, require_feature};
use crate::declarative::diff::diff;
use crate::declarative::kinds::{Live, World, adapter, check_route, readable, tree_of_doc};
use crate::declarative::plan::compare;
use crate::error::{ApiError, Json};
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct DriftCheckerConfig {
    /// 바뀜을 몰아 받는 간격(이 간격마다 revision이 바뀌었으면 다시 견준다)
    pub tick: Duration,
    /// 바뀜이 없어도 다시 견주는 간격
    pub full: Duration,
}

impl Default for DriftCheckerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(10),
            full: Duration::from_secs(300),
        }
    }
}

/// 드리프트 반복 작업. 모든 노드에서 띄우고 리더인 노드에서만 일한다.
pub struct DriftChecker {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl DriftChecker {
    pub fn spawn(state: Arc<AppState>, config: DriftCheckerConfig) -> Self {
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
    config: DriftCheckerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // (마지막으로 견준 revision, 그때)
    let mut last: Option<(u64, tokio::time::Instant)> = None;
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        let cluster = &state.cluster;
        if cluster.members().leader != Some(cluster.id()) {
            last = None;
            continue;
        }
        let revision = cluster.revision();
        if last.is_some_and(|(r, at)| r == revision && at.elapsed() < config.full) {
            continue;
        }
        tokio::select! {
            _ = &mut shutdown => return,
            done = Box::pin(tick(&state)) => {
                if done {
                    // 드리프트를 쓴 것도 revision을 올린다 — 쓴 뒤의 revision을 기억한다.
                    last = Some((state.cluster.revision(), tokio::time::Instant::now()));
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 모든 묶음을 한 번 견준다. 리더가 아니거나 읽지 못했으면 거짓.
pub(crate) async fn tick(state: &AppState) -> bool {
    let cluster = &state.cluster;
    if cluster.read_barrier().await.is_err() {
        return false;
    }
    if !missing_feature(cluster, DECLARATIVE).is_ok_and(|old| old.is_empty()) {
        return false;
    }
    let Ok((_, sets)) = cluster.list::<ApplySetSpec>() else {
        return false;
    };
    if sets.is_empty() {
        return true;
    }
    let Ok(world) = World::load(cluster) else {
        return false;
    };
    for set in sets {
        let items = match Box::pin(drift_items(state, &world, &set)).await {
            Ok(items) => items,
            Err(e) => {
                tracing::warn!(set = %set.name, error = %e.message, "cannot check the apply set drift");
                continue;
            }
        };
        let old = set
            .status
            .drift
            .as_ref()
            .map(|d| &d.items[..])
            .unwrap_or(&[]);
        if old == items.as_slice() && (set.status.drift.is_some() || items.is_empty()) {
            continue;
        }
        let mut status = set.status.clone();
        status.drift = Some(DriftReport {
            checked_at: now_ms(),
            revision: world.revision,
            items,
        });
        match cluster
            .update_status_checked::<ApplySetSpec>(set.id, set.revision, &status)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => {
                tracing::warn!(set = %set.name, error = %e, "cannot record the apply set drift")
            }
        }
    }
    true
}

/// 묶음 선언(`AppliedObject.declared`)을 매니페스트로. 평문이던 비밀 표시는 "그대로 둠" 참조(견주지 않는다).
pub(crate) fn declared_doc(
    kind: ManifestKind,
    name: &str,
    declared: &Value,
) -> Result<Manifest, String> {
    let mut meta = declared
        .get("metadata")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !meta.is_object() {
        meta = json!({});
    }
    meta["name"] = json!(name);
    let mut doc = json!({
        "apiVersion": opl_types::manifest::API_VERSION,
        "kind": kind,
        "metadata": meta,
        "spec": declared.get("spec").cloned().unwrap_or_else(|| json!({})),
    });
    if let Some(state) = declared.get("state") {
        doc["state"] = state.clone();
    }
    let mut m: Manifest = serde_json::from_value(doc).map_err(|e| e.to_string())?;
    if let Some(a) = adapter(kind) {
        for path in a.secret_paths() {
            let pointer = format!("/{}", path.replace('.', "/"));
            if let Some(v) = m.spec.pointer_mut(&pointer)
                && is_secret_mark(v)
            {
                *v = serde_json::to_value(SecretValue::reference(kind, name, path))
                    .expect("a secret reference serializes");
            }
        }
    }
    Ok(m)
}

fn is_secret_mark(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key(SECRET_MARK))
}

/// 선언한 폴더 경로의 지금 id(없으면 바깥 None, 뿌리면 Some(None))
fn declared_folder(world: &World, doc: &Manifest) -> Option<Option<uuid::Uuid>> {
    let tree = tree_of_doc(doc).ok().flatten();
    match (tree, doc.metadata.folder.as_deref()) {
        (Some(tree), Some(path)) if folder_path(path).is_ok_and(|p| !p.is_empty()) => {
            world.folder_id(tree, path).map(Some)
        }
        _ => Some(None),
    }
}

/// 다른 칸(저장 꼴 경로)과 레이블
fn object_drift(
    state: &AppState,
    world: &World,
    live: &Live,
    obj: &AppliedObject,
    set: &str,
) -> Vec<String> {
    let Ok(doc) = declared_doc(live.kind, &live.name, &obj.declared) else {
        return vec!["spec".into()];
    };
    let mut paths = Vec::new();
    let folder = match declared_folder(world, &doc) {
        Some(f) => f,
        None => {
            paths.push("metadata.folder".into());
            live.folder()
        }
    };
    match compare(state, world, live, &doc, folder, obj.declared.get("spec")) {
        Ok(Ok((_, _, changes))) => paths.extend(changes.into_iter().map(|c| c.path)),
        _ => paths.push("spec".into()),
    }
    for (k, v) in &doc.metadata.labels {
        if live.labels.get(k) != Some(v) {
            paths.push(format!("labels.{k}"));
        }
    }
    if live.labels.get(APPLY_SET_LABEL).map(String::as_str) != Some(set) {
        paths.push(format!("labels.{APPLY_SET_LABEL}"));
    }
    paths.sort();
    paths.dedup();
    paths
}

/// `path`(또는 그 위·아래 칸)를 바꾼 줄인지
fn touches(changed: &[String], path: &str) -> bool {
    changed.iter().any(|c| {
        c == path || c.starts_with(&format!("{path}.")) || path.starts_with(&format!("{c}."))
    })
}

fn history_ref(r: &HistoryRecord) -> HistoryRef {
    HistoryRef {
        revision: r.revision,
        at_ms: r.at_ms,
        actor: r.actor.clone(),
        source: r.source,
        reason: r.reason.clone(),
    }
}

/// 묶음이 적용한 뒤(`after`) `paths` 가운데 하나를 처음 바꾼 줄(지우기면 지운 줄)
fn first_change(
    records: &[HistoryRecord],
    after: u64,
    paths: &[String],
    deleted: bool,
) -> Option<HistoryRef> {
    for (i, r) in records.iter().enumerate() {
        if r.revision <= after {
            continue;
        }
        if deleted {
            if r.op == HistoryOp::Delete {
                return Some(history_ref(r));
            }
            continue;
        }
        let prev = i.checked_sub(1).map(|p| &records[p]);
        let mut changed: Vec<String> = match prev {
            Some(p) => diff(&p.spec, &r.spec).into_iter().map(|c| c.path).collect(),
            None => Vec::new(),
        };
        if let Some(p) = prev {
            for k in p.labels.keys().chain(r.labels.keys()) {
                if p.labels.get(k) != r.labels.get(k) {
                    changed.push(format!("labels.{k}"));
                }
            }
        }
        if paths.iter().any(|p| touches(&changed, p)) {
            return Some(history_ref(r));
        }
    }
    None
}

/// 묶음 하나의 드리프트
pub(crate) async fn drift_items(
    state: &AppState,
    world: &World,
    set: &ApplySet,
) -> Result<Vec<DriftItem>, ApiError> {
    let mut items = Vec::new();
    for obj in &set.status.objects {
        let Some(kind) = ManifestKind::from_kind(obj.kind).filter(|k| adapter(*k).is_some()) else {
            continue;
        };
        let records = state
            .cluster
            .history_of(obj.kind, obj.id)
            .unwrap_or_default();
        let Some(live) = world.by_id(kind, obj.id) else {
            items.push(DriftItem {
                kind: obj.kind,
                id: obj.id,
                name: obj.name.clone(),
                state: DriftState::Missing,
                paths: Vec::new(),
                by: first_change(&records, obj.revision, &[], true),
            });
            continue;
        };
        if live.revision == obj.revision {
            continue;
        }
        let paths = object_drift(state, world, live, obj, &set.name);
        if paths.is_empty() {
            continue;
        }
        let unmanaged = paths.len() == 1 && paths[0] == format!("labels.{APPLY_SET_LABEL}");
        items.push(DriftItem {
            kind: obj.kind,
            id: obj.id,
            name: live.name.clone(),
            state: if unmanaged {
                DriftState::Unmanaged
            } else {
                DriftState::Modified
            },
            by: first_change(&records, obj.revision, &paths, false),
            paths,
        });
    }
    Ok(items)
}

/// 선언한 칸의 모양대로 지금 값을 옮긴다(선언하지 않은 칸은 선언에 넣지 않는다 — 세 갈래가 그대로다).
fn project(declared: &Value, live: &Value) -> Value {
    if is_secret_mark(declared) {
        return declared.clone();
    }
    match (declared, live) {
        (Value::Object(d), Value::Object(l)) => Value::Object(
            d.iter()
                .filter_map(|(k, dv)| match l.get(k) {
                    Some(lv) if !lv.is_null() => Some((k.clone(), project(dv, lv))),
                    _ => None,
                })
                .collect(),
        ),
        _ => live.clone(),
    }
}

/// 받아들인 선언: 선언 칸마다 지금 값
fn adopted(declared: &Value, now: &Manifest) -> Value {
    let mut out = declared.clone();
    let spec = declared.get("spec").cloned().unwrap_or_else(|| json!({}));
    out["spec"] = project(&spec, &now.spec);
    if let Some(meta) = out.get_mut("metadata").and_then(Value::as_object_mut) {
        if meta.contains_key("folder") {
            match &now.metadata.folder {
                Some(f) => {
                    meta.insert("folder".into(), json!(f));
                }
                None => {
                    meta.remove("folder");
                }
            }
        }
        if let Some(labels) = meta.get_mut("labels").and_then(Value::as_object_mut) {
            let keys: Vec<String> = labels.keys().cloned().collect();
            for k in keys {
                match now.metadata.labels.get(&k) {
                    Some(v) => {
                        labels.insert(k, json!(v));
                    }
                    None => {
                        labels.remove(&k);
                    }
                }
            }
        }
    }
    if out.get("state").is_some() {
        match now.state {
            Some(s) => out["state"] = serde_json::to_value(s).expect("state"),
            None => {
                out.as_object_mut().map(|o| o.remove("state"));
            }
        }
    }
    out
}

/// `POST /apply-sets/{key}/adopt`의 본문
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptRequest {
    /// 받아들일 객체(`종류/이름` 또는 id). 비면 드리프트 보고에서 볼 수 있고 바꿀 수 있는 객체 모두(밖에서 지운 것 포함)
    #[serde(default)]
    pub objects: Vec<String>,
}

/// 실제를 선언으로 받아들인다(§12.5): 고른 객체(없으면 드리프트 보고에서 볼 수 있고 바꿀 수 있는 모두)의 묶음 선언을 지금
/// 값으로 바꾼다 — 리소스는 바꾸지 않는다. 밖에서 지운 객체는 묶음에서 뺀다. 그 뒤 `orbivisorctl export --set`으로 git의
/// 파일을 맞춘다. 고른 객체 모두의 바꾸기 권한이 든다(하나라도 없으면 403, 아무것도 바꾸지 않음).
#[utoipa::path(post, path = "/apply-sets/{key}/adopt", tag = "declarative",
    params(("key" = String, Path, description = "id or name")),
    request_body = AdoptRequest,
    responses((status = 200, body = opl_types::resource::Resource<ApplySetSpec, ApplySetStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn adopt(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<AdoptRequest>,
) -> Result<Json<ApplySet>, ApiError> {
    require_feature(&state.cluster, DECLARATIVE)?;
    state
        .authz
        .require_anywhere(&principal, Privilege::SystemRead)?;
    for _ in 0..5 {
        let set = crate::resources::lookup::<ApplySetSpec>(&state.cluster, &key)?;
        let world = World::load(&state.cluster)?;
        let drifted: Vec<uuid::Uuid> = set
            .status
            .drift
            .as_ref()
            .map(|d| d.items.iter().map(|i| i.id).collect())
            .unwrap_or_default();
        let chosen: Vec<&AppliedObject> = if req.objects.is_empty() {
            // 모두 = 이 주체가 볼 수 있고 바꿀 수 있는 드리프트 객체(밖에서 지운 것 포함 — 리뷰 1: 폴더 하나만 맡은
            // 운영자도 보이는 것을 모두 받아들인다. 보이지 않는 것은 그대로 둔다)
            set.status
                .objects
                .iter()
                .filter(|o| drifted.contains(&o.id))
                .filter(|o| {
                    let Some(kind) = ManifestKind::from_kind(o.kind) else {
                        return false;
                    };
                    match (adapter(kind), world.by_id(kind, o.id)) {
                        (Some(a), Some(live)) => {
                            readable(&state, &principal, live)
                                && check_route(&state, &principal, a.update_route(), Some(live.id))
                                    .is_ok()
                        }
                        (_, None) => true,
                        (None, Some(_)) => false,
                    }
                })
                .collect()
        } else {
            let mut out = Vec::new();
            for want in &req.objects {
                let found = set.status.objects.iter().find(|o| {
                    o.id.to_string() == *want
                        || want.split_once('/').is_some_and(|(k, n)| {
                            n == o.name
                                && ManifestKind::from_kind(o.kind)
                                    .is_some_and(|mk| mk.as_str() == k || o.kind.as_str() == k)
                        })
                });
                out.push(found.ok_or_else(|| {
                    ApiError::not_found(format!("apply set {:?} has no object {want:?}", set.name))
                })?);
            }
            out
        };
        // 권한 먼저(하나라도 없으면 아무것도 바꾸지 않는다)
        for o in &chosen {
            let Some(kind) = ManifestKind::from_kind(o.kind) else {
                continue;
            };
            if let (Some(a), Some(live)) = (adapter(kind), world.by_id(kind, o.id)) {
                check_route(&state, &principal, a.update_route(), Some(live.id))
                    .map_err(|_| ApiError::forbidden())?;
            }
        }
        let mut status = set.status.clone();
        let mut adopted_n = 0;
        status.objects = Vec::new();
        for o in &set.status.objects {
            if !chosen.iter().any(|c| c.id == o.id && c.kind == o.kind) {
                status.objects.push(o.clone());
                continue;
            }
            adopted_n += 1;
            let Some(live) = ManifestKind::from_kind(o.kind).and_then(|k| world.by_id(k, o.id))
            else {
                continue; // 밖에서 지운 것은 묶음에서 뺀다.
            };
            let now = world.manifest_of(live);
            status.objects.push(AppliedObject {
                kind: o.kind,
                id: o.id,
                name: live.name.clone(),
                declared: adopted(&o.declared, &now),
                revision: live.revision,
            });
        }
        if let Some(d) = status.drift.as_mut() {
            d.items.retain(|i| !chosen.iter().any(|c| c.id == i.id));
        }
        status
            .check_limits()
            .map_err(|e| ApiError::bad_request(e.to_string()).with_code("apply_set_too_large"))?;
        match state
            .cluster
            .update_status_checked::<ApplySetSpec>(set.id, set.revision, &status)
            .await
        {
            Ok(_) => {
                audit.detail(format!(
                    "apply-set adopt {} ({adopted_n} objects)",
                    set.name
                ));
                let set =
                    crate::resources::lookup::<ApplySetSpec>(&state.cluster, &set.id.to_string())?;
                return Ok(Json(crate::declarative::visible_set(
                    &state, &principal, set,
                )?));
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the apply set kept changing; try again",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopting_keeps_the_declared_shape() {
        let declared = json!({
            "metadata": { "folder": "/prod", "labels": { "tier": "web", "gone": "x" } },
            "spec": { "cpu": { "cores": 2 }, "memory_mib": 512, "cloud_init": { "password": { "$secret": "set" } } },
            "state": "running",
        });
        let mut now = Manifest::new(
            ManifestKind::Vm,
            "web",
            json!({ "cpu": { "cores": 4, "sockets": 1 }, "memory_mib": 1024, "machine": "virt",
                "cloud_init": { "password": { "secretRef": "Vm/web/cloud_init.password" } } }),
        );
        now.metadata.folder = Some("/test".into());
        now.metadata.labels.insert("tier".into(), "db".into());
        now.state = Some(opl_types::manifest::ManifestState::Stopped);
        let out = adopted(&declared, &now);
        assert_eq!(
            out,
            json!({
                "metadata": { "folder": "/test", "labels": { "tier": "db" } },
                "spec": { "cpu": { "cores": 4 }, "memory_mib": 1024, "cloud_init": { "password": { "$secret": "set" } } },
                "state": "stopped",
            })
        );
    }

    #[test]
    fn the_first_row_that_touched_a_path_is_blamed() {
        let meta = opl_types::history::ChangeMeta::default();
        let row = |rev, op, spec: Value| {
            HistoryRecord::new(
                &meta,
                rev,
                opl_types::resource::Kind::Vm,
                uuid::Uuid::nil(),
                "web".into(),
                op,
                Default::default(),
                spec,
            )
        };
        let rows = vec![
            row(
                10,
                HistoryOp::Create,
                json!({ "cpu": { "cores": 1 }, "memory_mib": 512 }),
            ),
            row(
                20,
                HistoryOp::Update,
                json!({ "cpu": { "cores": 1 }, "memory_mib": 1024 }),
            ),
            row(
                30,
                HistoryOp::Update,
                json!({ "cpu": { "cores": 2 }, "memory_mib": 1024 }),
            ),
        ];
        let by = |paths: &[&str]| {
            let paths: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
            first_change(&rows, 10, &paths, false).map(|r| r.revision)
        };
        assert_eq!(by(&["cpu.cores"]), Some(30));
        assert_eq!(by(&["cpu"]), Some(30));
        assert_eq!(by(&["memory_mib", "cpu.cores"]), Some(20));
        assert_eq!(by(&["machine"]), None);
        assert_eq!(first_change(&rows, 20, &["memory_mib".into()], false), None);
    }
}
