//! 종류마다 선언 어댑터(계획 73 §8.1). 어댑터는 매니페스트 spec(참조는 이름·폴더 경로)과 저장 spec(참조는 id)을 잇고,
//! 쓰기는 그 종류의 지금 처리기를 그대로 부른다 — 검증·권한·할당량·독점 키·감사 메모·Task 만들기를 비껴가지 않는다.
//! 처리기 앞의 경로표 권한은 [`check_route`]가 같은 표로 본다.

mod floating_ip;
mod folder;
mod network;
mod placement_rule;
mod router;
mod security_group;
mod storage_pool;
mod vm;
mod volume;

pub(crate) use vm::needs_restart as vm_needs_restart;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::http::header::IF_MATCH;
use axum::http::{HeaderMap, HeaderValue, Method};
use opl_cluster::{Cluster, Typed};
use opl_types::folder::{FolderKind, FolderSpec};
use opl_types::manifest::{Manifest, ManifestKind, ManifestState, SecretValue, folder_path};
use opl_types::node::NodeSpec;
use opl_types::placement::PlacementRuleSpec;
use opl_types::resource::ResourceSpec;
use opl_types::storage::VolumeSpec;
use opl_types::vm::VmSpec;
use serde_json::Value;
use uuid::Uuid;

use super::Problem;
use crate::auth::{Action, Principal};
use crate::authz_routes::{Obj, check, resolve, rule};
use crate::error::ApiError;
use crate::resources::CreateRequest;
use crate::state::AppState;

pub(crate) type Fut<'a, T> = Pin<Box<dyn Future<Output = Result<T, ApiError>> + Send + 'a>>;

/// 적용 차례(같은 층 안의 종류 차례 — §8.1 표): 폴더·풀·네트워크·보안 그룹 → 라우터 → 볼륨 → VM → 유동 IP·배치 규칙
pub(crate) const ORDER: [ManifestKind; 9] = [
    ManifestKind::Folder,
    ManifestKind::StoragePool,
    ManifestKind::Network,
    ManifestKind::SecurityGroup,
    ManifestKind::Router,
    ManifestKind::Volume,
    ManifestKind::Vm,
    ManifestKind::FloatingIp,
    ManifestKind::PlacementRule,
];

/// 적용 차례에서의 자리(다루지 않는 종류는 끝)
pub(crate) fn rank(kind: ManifestKind) -> usize {
    ORDER.iter().position(|k| *k == kind).unwrap_or(ORDER.len())
}

/// 종류의 어댑터. 이 스테이지가 다루지 않는 종류(백업·HA·권한·알림·realm·설정·ACME — 계획 73 스테이지 3·4)는 None.
pub(crate) fn adapter(kind: ManifestKind) -> Option<&'static dyn Adapter> {
    Some(match kind {
        ManifestKind::Folder => &folder::FolderAdapter,
        ManifestKind::StoragePool => &storage_pool::PoolAdapter,
        ManifestKind::Network => &network::NetworkAdapter,
        ManifestKind::SecurityGroup => &security_group::GroupAdapter,
        ManifestKind::Router => &router::RouterAdapter,
        ManifestKind::Volume => &volume::VolumeAdapter,
        ManifestKind::Vm => &vm::VmAdapter,
        ManifestKind::FloatingIp => &floating_ip::FipAdapter,
        ManifestKind::PlacementRule => &placement_rule::RuleAdapter,
        _ => return None,
    })
}

/// 지금 있는 객체 하나(저장 꼴)
#[derive(Debug, Clone)]
pub(crate) struct Live {
    pub kind: ManifestKind,
    pub id: Uuid,
    /// 리소스 이름(폴더는 화면 이름 — 폴더 리소스의 이름은 id다)
    pub name: String,
    pub revision: u64,
    pub labels: BTreeMap<String, String>,
    /// 저장한 spec(JSON)
    pub spec: Value,
}

impl Live {
    pub(crate) fn of<S: ResourceSpec>(kind: ManifestKind, r: &Typed<S>) -> Self {
        Self {
            kind,
            id: r.id,
            name: r.name.clone(),
            revision: r.revision,
            labels: r.labels.clone(),
            spec: serde_json::to_value(&r.spec).expect("a stored spec serializes"),
        }
    }

    /// 든 폴더(폴더는 부모)
    pub(crate) fn folder(&self) -> Option<Uuid> {
        let field = if self.kind == ManifestKind::Folder {
            "parent"
        } else {
            "folder"
        };
        self.spec
            .get(field)
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok())
    }
}

/// 처리기를 부른 결과: 만든(바꾼) 객체와 그 쓰기가 만든 Task(기다린다)
pub(crate) struct Done {
    pub id: Uuid,
    pub task: Option<Uuid>,
}

/// 바꾸기 길
pub(crate) enum Update {
    /// 통째(PUT — 레이블도 싣는다)
    Whole,
    /// 이 맨 위 칸만 바꿀 수 있다(PATCH). 다른 칸이 다르면 막힘 `immutable`.
    Fields(&'static [&'static str]),
}

/// 같은 계획 안에서 먼저 만들 것(VM 미리 확인이 얹는다 — 계획 71 `Planned`)과 계획의 문서
#[derive(Default)]
pub(crate) struct Planned {
    pub volumes: Vec<CreateRequest<VolumeSpec>>,
    pub rules: Vec<CreateRequest<PlacementRuleSpec>>,
    /// (종류, 문서 열쇠) → 문서
    pub docs: BTreeMap<(ManifestKind, String), Manifest>,
}

/// 미리 확인 하나의 재료
pub(crate) struct Check<'a> {
    pub state: &'a Arc<AppState>,
    pub principal: &'a Principal,
    pub name: &'a str,
    pub labels: &'a BTreeMap<String, String>,
    /// 원하는 저장 spec(만들기는 선언 그대로 + 기본값, 바꾸기는 정규화한 것)
    pub desired: &'a Value,
    pub live: Option<&'a Live>,
    pub planned: &'a Planned,
}

pub(crate) trait Adapter: Sync {
    /// 든 폴더 트리(폴더를 따르지 않으면 None — `metadata.folder`를 받지 않는다)
    fn tree(&self) -> Option<FolderKind> {
        None
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError>;

    /// 저장 spec → 매니페스트 spec. 기본: 폴더 칸을 뺀다(폴더는 `metadata.folder`).
    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        without(spec, &["folder"])
    }

    /// 매니페스트 spec → 저장 spec(이름 → id). 기본: 폴더 id를 넣는다.
    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        Ok(with(
            spec,
            "folder",
            folder.map(|f| Value::String(f.to_string())),
        ))
    }

    /// 저장 spec을 그 종류의 타입으로 풀어 다시 쓴다(기본값이 채워진다). 칸 이름이 틀리거나 빠졌으면 오류다.
    fn normalize(&self, store: &Value) -> Result<Value, String>;

    /// 서버가 정한 칸(MAC·주소·노드·기반 볼륨)을 선언하지 않았으면 지금 값에서 가져온다. `declared`는 매니페스트 spec이다.
    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        _desired: &mut Value,
        _live: &Value,
    ) {
    }

    /// 만들기 전에 채우는 기본값(VM 머신·볼륨 형식)
    fn create_defaults(
        &self,
        _state: &AppState,
        _world: &World,
        _planned: &Planned,
        _desired: &mut Value,
    ) -> Result<(), ApiError> {
        Ok(())
    }

    fn updatable(&self) -> Update;

    /// 참조(종류, 문서 열쇠): 같은 계획 안이면 그 단계 뒤에 하고, 아니면 지금 있어야(볼 수 있어야) 한다.
    fn refs(&self, _spec: &Value) -> Vec<(ManifestKind, String)> {
        Vec::new()
    }

    /// 비밀 칸(계획의 diff에 값 대신 `(changed)`)
    fn secret_paths(&self) -> &'static [&'static str] {
        &[]
    }

    /// 만들 권한. `folder`는 든 폴더(아직 없으면 있는 가장 가까운 조상)다.
    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError>;

    /// 바꾸기·지우기 경로(경로표 규칙을 본다)
    fn update_route(&self) -> (&'static str, &'static str);
    fn delete_route(&self) -> (&'static str, &'static str);

    /// 경로표가 `Handler`라 처리기가 보는 바꾸기 권한(폴더 할당량·유동 IP)
    fn may_update_extra(
        &self,
        _state: &AppState,
        _principal: &Principal,
        _live: &Live,
    ) -> Result<(), ApiError> {
        Ok(())
    }

    /// 쓰지 않는 미리 확인(문제 목록). 기본은 저장 spec 검증뿐이다(처리기가 같은 검사를 다시 한다).
    fn precheck<'a>(&'a self, _check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        name: String,
        labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done>;

    /// 바꾼다. `fields`는 바뀐 맨 위 칸이다(`Update::Fields`면 그 칸만 보낸다).
    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        labels: BTreeMap<String, String>,
        desired: Value,
        fields: Vec<String>,
    ) -> Fut<'a, Done>;

    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done>;

    /// 레이블만 쓴다(PATCH 본문이 레이블을 싣지 않는 종류 — 적용 묶음 레이블·선언한 레이블). spec은 그대로다.
    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()>;

    /// 바꾸기 길이 레이블을 싣는지(아니면 [`Adapter::write_labels`]로 따로)
    fn update_carries_labels(&self) -> bool {
        false
    }

    /// 만들기 길이 레이블을 싣는지(폴더는 싣지 않는다)
    fn create_carries_labels(&self) -> bool {
        true
    }
}

/// `spec`에서 칸들을 뺀 것
pub(crate) fn without(spec: &Value, fields: &[&str]) -> Value {
    let mut out = spec.clone();
    if let Some(m) = out.as_object_mut() {
        for f in fields {
            m.remove(*f);
        }
    }
    out
}

/// `spec`의 칸 `field`를 `value`로(None이면 뺀다)
pub(crate) fn with(spec: &Value, field: &str, value: Option<Value>) -> Value {
    let mut out = match spec {
        Value::Object(_) => spec.clone(),
        _ => Value::Object(Default::default()),
    };
    let m = out.as_object_mut().expect("object");
    match value {
        Some(v) => {
            m.insert(field.into(), v);
        }
        None => {
            m.remove(field);
        }
    }
    out
}

/// 저장 spec을 `S`로 풀어 다시 쓴다.
pub(crate) fn roundtrip<S: ResourceSpec>(v: &Value) -> Result<Value, String> {
    Ok(serde_json::to_value(typed::<S>(v)?).expect("a spec serializes"))
}

pub(crate) fn typed<S: serde::de::DeserializeOwned>(v: &Value) -> Result<S, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

/// 400(본문을 풀지 못함)
pub(crate) fn bad(e: String) -> ApiError {
    ApiError::bad_request(format!("spec: {e}"))
}

pub(crate) fn lives_of<S: ResourceSpec>(
    cluster: &Cluster,
    kind: ManifestKind,
) -> Result<Vec<Live>, ApiError> {
    Ok(cluster
        .list::<S>()?
        .1
        .iter()
        .map(|r| Live::of(kind, r))
        .collect())
}

/// 레이블만 쓴다: 읽은 revision이 그대로일 때 같은 spec에 새 레이블(바뀌었으면 412).
pub(crate) fn write_labels_as<'a, S: ResourceSpec>(
    cluster: &'a Cluster,
    id: Uuid,
    revision: u64,
    labels: BTreeMap<String, String>,
) -> Fut<'a, ()> {
    Box::pin(async move {
        let Some(current) = cluster.get::<S>(id)? else {
            return Err(ApiError::not_found(format!(
                "{} {id} not found",
                S::KIND.as_str()
            )));
        };
        cluster
            .update_spec(id, Some(revision), labels, &current.spec)
            .await?;
        Ok(())
    })
}

/// `If-Match: revision` 머리
pub(crate) fn if_match(revision: u64) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(IF_MATCH, HeaderValue::from(revision));
    h
}

/// 경로표의 (메서드, 경로) 규칙을 `key` 객체로 본다(처리기 앞의 미들웨어와 같다).
pub(crate) fn check_route(
    state: &AppState,
    principal: &Principal,
    (method, template): (&str, &str),
    key: Option<Uuid>,
) -> Result<(), ApiError> {
    let m = Method::from_bytes(method.as_bytes()).map_err(ApiError::internal)?;
    let r = rule(&m, template)
        .ok_or_else(|| ApiError::internal(format!("no route rule for {method} {template}")))?;
    let key = key.map(|k| k.to_string());
    check(state, principal, r, key.as_deref())
}

/// 경로표가 객체를 찾는 종류
fn obj(kind: ManifestKind) -> Option<Obj> {
    Some(match kind {
        ManifestKind::Vm => Obj::Vm,
        ManifestKind::Volume => Obj::Volume,
        ManifestKind::Network => Obj::Network,
        ManifestKind::StoragePool => Obj::StoragePool,
        ManifestKind::Folder => Obj::Folder,
        ManifestKind::Router => Obj::Router,
        ManifestKind::FloatingIp => Obj::FloatingIp,
        _ => return None,
    })
}

/// 주체가 이 객체를 볼 수 있는지(목록·조회와 같은 규칙). 볼 수 없는 것은 없는 것과 같게 다룬다.
pub(crate) fn readable(state: &AppState, principal: &Principal, live: &Live) -> bool {
    match obj(live.kind) {
        Some(o) => match resolve(&state.cluster, o, &live.id.to_string()) {
            Ok(Some(found)) => state
                .authz
                .effective(principal, &found)
                .is_ok_and(|set| set.contains(found.owned.read)),
            _ => false,
        },
        None => state
            .authorize(principal, Action::Read, live.kind.kind())
            .is_ok(),
    }
}

/// 트리 이름(`vm`·`network`·`storage`)
pub(crate) fn tree_name(tree: FolderKind) -> &'static str {
    match tree {
        FolderKind::Vm => "vm",
        FolderKind::Network => "network",
        FolderKind::Storage => "storage",
    }
}

/// 폴더 문서의 열쇠(`vm:/prod/web`)
pub(crate) fn folder_key(tree: FolderKind, path: &str) -> String {
    format!("{}:{path}", tree_name(tree))
}

/// 경로에 이름 하나를 잇는다(`/` + `web` → `/web`).
pub(crate) fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// 계획·적용·내보내기가 보는 클러스터(읽은 때의 것)
pub(crate) struct World {
    pub revision: u64,
    /// id → (이름, 부모, 트리)
    folders: BTreeMap<Uuid, (String, Option<Uuid>, FolderKind)>,
    nodes: Vec<(Uuid, String)>,
    vms: Vec<(Uuid, String)>,
    /// (종류, 문서 열쇠) → 객체
    pub objects: BTreeMap<(ManifestKind, String), Live>,
}

impl World {
    pub(crate) fn load(cluster: &Cluster) -> Result<Self, ApiError> {
        let revision = cluster.revision();
        let folders = cluster
            .list::<FolderSpec>()?
            .1
            .into_iter()
            .map(|f| (f.id, (f.spec.name, f.spec.parent, f.spec.kind)))
            .collect();
        let nodes = cluster
            .list::<NodeSpec>()?
            .1
            .into_iter()
            .map(|n| (n.id, n.name))
            .collect();
        let vms = cluster
            .list::<VmSpec>()?
            .1
            .into_iter()
            .map(|v| (v.id, v.name))
            .collect();
        let mut world = Self {
            revision,
            folders,
            nodes,
            vms,
            objects: BTreeMap::new(),
        };
        for kind in ORDER {
            let a = adapter(kind).expect("ORDER kinds have adapters");
            for live in a.lives(cluster)? {
                let key = world.key_of_live(&live);
                world.objects.insert((kind, key), live);
            }
        }
        Ok(world)
    }

    /// 폴더 id의 경로(`/prod/web`). 모르는 폴더면 None.
    pub(crate) fn folder_path(&self, id: Uuid) -> Option<String> {
        let mut parts = Vec::new();
        let mut next = Some(id);
        while let Some(f) = next {
            let (name, parent, _) = self.folders.get(&f)?;
            parts.push(name.clone());
            next = *parent;
            if parts.len() > opl_types::folder::MAX_FOLDER_DEPTH {
                return None;
            }
        }
        parts.reverse();
        Some(format!("/{}", parts.join("/")))
    }

    pub(crate) fn folder_tree(&self, id: Uuid) -> Option<FolderKind> {
        self.folders.get(&id).map(|f| f.2)
    }

    /// 트리 `tree`의 경로 `path`인 폴더(이름은 대소문자를 가리지 않는다 — 형제 이름 규칙과 같다). 뿌리(`/`)는 None.
    pub(crate) fn folder_id(&self, tree: FolderKind, path: &str) -> Option<Uuid> {
        let parts = folder_path(path).ok()?;
        let mut at: Option<Uuid> = None;
        for part in parts {
            at = Some(self.child(tree, at, part)?);
        }
        at
    }

    /// 경로에서 있는 가장 깊은 폴더(아직 없는 폴더에 만들 권한을 볼 때)
    pub(crate) fn nearest_folder(&self, tree: FolderKind, path: &str) -> Option<Uuid> {
        let parts = folder_path(path).ok()?;
        let mut at: Option<Uuid> = None;
        for part in parts {
            match self.child(tree, at, part) {
                Some(id) => at = Some(id),
                None => break,
            }
        }
        at
    }

    fn child(&self, tree: FolderKind, parent: Option<Uuid>, name: &str) -> Option<Uuid> {
        self.folders
            .iter()
            .find(|(_, (n, p, k))| *k == tree && *p == parent && n.eq_ignore_ascii_case(name))
            .map(|(id, _)| *id)
    }

    pub(crate) fn node_id(&self, name: &str) -> Option<Uuid> {
        if let Ok(id) = name.parse::<Uuid>() {
            return self.nodes.iter().any(|(n, _)| *n == id).then_some(id);
        }
        self.nodes
            .iter()
            .find(|(_, n)| n == name)
            .map(|(id, _)| *id)
    }

    pub(crate) fn node_name(&self, id: Uuid) -> String {
        self.nodes
            .iter()
            .find(|(n, _)| *n == id)
            .map_or_else(|| id.to_string(), |(_, name)| name.clone())
    }

    pub(crate) fn vm_id(&self, name: &str) -> Option<Uuid> {
        if let Ok(id) = name.parse::<Uuid>() {
            return Some(id);
        }
        self.vms.iter().find(|(_, n)| n == name).map(|(id, _)| *id)
    }

    pub(crate) fn vm_name(&self, id: Uuid) -> String {
        self.vms
            .iter()
            .find(|(v, _)| *v == id)
            .map_or_else(|| id.to_string(), |(_, name)| name.clone())
    }

    /// 지금 객체의 문서 열쇠(폴더는 `트리:경로`, 그 밖은 이름)
    pub(crate) fn key_of_live(&self, live: &Live) -> String {
        if live.kind == ManifestKind::Folder {
            let tree = self.folder_tree(live.id).unwrap_or(FolderKind::Vm);
            let path = self
                .folder_path(live.id)
                .unwrap_or_else(|| live.id.to_string());
            return folder_key(tree, &path);
        }
        live.name.clone()
    }

    /// id로 객체 찾기
    pub(crate) fn by_id(&self, kind: ManifestKind, id: Uuid) -> Option<&Live> {
        self.objects
            .iter()
            .find(|((k, _), l)| *k == kind && l.id == id)
            .map(|(_, l)| l)
    }

    /// 지금 객체의 매니페스트(id → 이름, 폴더 → 경로). 서버가 다는 `orbivisor.io/` 레이블은 뺀다.
    pub(crate) fn manifest_of(&self, live: &Live) -> Manifest {
        let a = adapter(live.kind).expect("adapter");
        let mut spec = a.to_manifest(&live.spec, self);
        // 비밀 칸은 "그대로 둠" 참조(`<종류>/<이름>/<칸>`) — 다시 적용해도 비밀이 바뀌지 않는다.
        for path in a.secret_paths() {
            let pointer = format!("/{}", path.replace('.', "/"));
            if let Some(v) = spec.pointer_mut(&pointer)
                && v.is_object()
            {
                *v = serde_json::to_value(SecretValue::reference(live.kind, &live.name, path))
                    .expect("a secret reference serializes");
            }
        }
        let mut m = Manifest::new(live.kind, live.name.clone(), Value::Null);
        m.metadata.folder = live.folder().and_then(|f| self.folder_path(f));
        m.metadata.labels = live
            .labels
            .iter()
            .filter(|(k, _)| !k.starts_with("orbivisor.io/"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if live.kind == ManifestKind::Vm {
            m.state = vm::state_of(&live.spec);
        }
        m.spec = spec;
        m
    }
}

/// 문서의 열쇠(폴더는 `트리:경로` — 트리는 `spec.kind`, 그 밖은 이름)
pub(crate) fn key_of_doc(doc: &Manifest) -> Result<String, String> {
    if doc.kind == ManifestKind::Folder {
        let tree = folder::tree_of(&doc.spec)?;
        let parent = doc.metadata.folder.as_deref().unwrap_or("/");
        return Ok(folder_key(tree, &join_path(parent, &doc.metadata.name)));
    }
    Ok(doc.metadata.name.clone())
}

/// 문서가 든 폴더 트리(폴더 문서는 자기 트리)
pub(crate) fn tree_of_doc(doc: &Manifest) -> Result<Option<FolderKind>, String> {
    if doc.kind == ManifestKind::Folder {
        return folder::tree_of(&doc.spec).map(Some);
    }
    Ok(adapter(doc.kind).and_then(|a| a.tree()))
}

/// 값에서 문자열 목록(`a[].b`)을 모은다.
pub(crate) fn names_at(spec: &Value, list: &str, field: &str) -> Vec<String> {
    spec.get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| item.get(field).and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// 이름 목록을 (종류, 이름) 참조로(겹침 없이)
pub(crate) fn refs_of(
    kind: ManifestKind,
    names: impl IntoIterator<Item = String>,
) -> Vec<(ManifestKind, String)> {
    let set: BTreeSet<String> = names.into_iter().collect();
    set.into_iter().map(|n| (kind, n)).collect()
}

/// 처리기 오류를 미리 확인 문제로
pub(crate) fn problem(path: Option<&str>, e: ApiError) -> Problem {
    Problem::error(e.code, e.message, path.map(str::to_string))
}
