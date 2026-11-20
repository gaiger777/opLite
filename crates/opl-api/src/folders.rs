//! 인벤토리 폴더 API(계획 62 설계 §5·§13): 종류별 트리(VM·네트워크·스토리지 풀)의 폴더 만들기·이름 바꾸기·옮기기·
//! 지우기와 리소스 옮기기. 부모·순환·깊이·형제 이름·빈 폴더는 상태 머신이 본다(`opl-cluster` `folders.rs`). 쓰기는 모든
//! 노드가 `folders`를 알 때만 받는다(409 `cluster_upgrading`).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::authz::Privilege;
use opl_types::authz_eval::AuthzTarget;
use opl_types::folder::{Folder, FolderKind, FolderQuota, FolderSpec, FolderStatus, MAX_FOLDERS};
use opl_types::network::NetworkSpec;
use opl_types::node::{FOLDERS, NUMA};
use opl_types::resource::{Kind, Resource};
use opl_types::storage::StoragePoolSpec;
use opl_types::vm::VmSpec;
use serde::{Deserialize, Deserializer, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz::{Found, Owned};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, if_match, lookup};
use crate::state::AppState;

/// 폴더의 경로(`/teamA/prod`). 없는 폴더(지워진 부모)에서 멈추고 그 id를 쓴다.
pub(crate) fn folder_path(cluster: &Cluster, id: Uuid) -> String {
    let mut parts = Vec::new();
    let mut next = Some(id);
    while let Some(f) = next {
        if parts.len() > opl_types::folder::MAX_FOLDER_DEPTH {
            break;
        }
        match cluster.get::<FolderSpec>(f).ok().flatten() {
            Some(folder) => {
                parts.push(folder.spec.name);
                next = folder.spec.parent;
            }
            None => {
                parts.push(f.to_string());
                break;
            }
        }
    }
    parts.reverse();
    format!("/{}", parts.join("/"))
}

/// 부모(없으면 그 트리의 루트)라는 대상
fn parent_target(parent: Option<Uuid>) -> AuthzTarget {
    match parent {
        Some(p) => AuthzTarget::Folder(p),
        None => AuthzTarget::Root,
    }
}

/// 폴더 옮기기(리뷰 2 N1): 옮기는 하위 트리 전체를 다른 관리 아래로 넘기므로 옛 부모와 새 부모 **안 모두**에서
/// `inventory.move`가 있어야 한다. 안 모두에 닿는 권한은 그 부모나 조상의 propagate grant뿐이라(grant 위임의
/// `check_delegation`과 같은 셈) 부모 안 객체 층으로 본다. 옛 부모 안이면 옮기는 폴더 안도 덮는다. 그래서 폴더 자체에만
/// 닿는(propagate가 아닌) `inventory.move`로 그 폴더를 자기가 다스리는 폴더 아래로 끌어오지 못한다(403).
fn require_move_inside(
    state: &AppState,
    principal: &Principal,
    folder: &Folder,
    new_parent: Option<Uuid>,
) -> Result<(), ApiError> {
    let kind = folder.spec.kind.member_kinds()[0];
    for parent in [folder.spec.parent, new_parent] {
        let inside = AuthzTarget::Object {
            kind,
            id: Uuid::nil(),
            folder: parent,
        };
        if !state
            .authz
            .allows(principal, Privilege::InventoryMove, inside)
        {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!(
                    "moving a folder needs inventory.move everywhere inside {} (a propagating grant there or above)",
                    parent.map_or_else(|| "/".to_string(), |p| folder_path(&state.cluster, p))
                ),
            ));
        }
    }
    Ok(())
}

/// 대상에서 `privilege`. 볼 수 없는 폴더는 없는 폴더와 같은 404다.
fn require_at(
    state: &AppState,
    principal: &Principal,
    privilege: Privilege,
    target: AuthzTarget,
) -> Result<(), ApiError> {
    match target {
        AuthzTarget::Root => state.authz.require_root(principal, privilege),
        AuthzTarget::Folder(f) => state.authz.require(
            principal,
            privilege,
            &Found {
                owned: Owned::object(target),
                missing: format!("folder {f} not found"),
            },
        ),
        AuthzTarget::Object { .. } => state.authz.require(principal, privilege, &target),
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct FolderQuery {
    /// 이 트리의 폴더만(`vm`, `network`, `storage`)
    #[param(inline)]
    pub kind: Option<FolderKind>,
}

/// 볼 수 있는 폴더의 평면 목록(부모 id로 트리를 이룬다). 부모를 볼 수 없는 폴더는 보는 사람에게 맨 위 폴더다.
/// 할당량 `dedicated_vcpus`(계획 64)는 옛 노드가 풀지 못한다.
fn require_quota_feature(state: &AppState, quota: Option<&FolderQuota>) -> Result<(), ApiError> {
    if quota.is_some_and(|q| q.dedicated_vcpus.is_some()) {
        require_feature(&state.cluster, NUMA)?;
    }
    // 네트워크 폴더의 라우터·유동 IP 수(계획 66)는 API 스테이지 전까지 막는다.
    if quota.is_some_and(|q| q.routers.is_some() || q.floating_ips.is_some()) {
        return Err(crate::sdn::routing_not_yet(
            &state.cluster,
            "routers and floating_ips quotas",
        ));
    }
    Ok(())
}

#[utoipa::path(get, path = "/folders", tag = "folders",
    params(FolderQuery),
    responses((status = 200, body = List<Resource<FolderSpec, FolderStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_folders(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<FolderQuery>,
) -> Result<Json<List<Folder>>, ApiError> {
    let (revision, folders) = state.cluster.list::<FolderSpec>()?;
    let folders = folders
        .into_iter()
        .filter(|f| q.kind.is_none_or(|k| f.spec.kind == k));
    let items = state.authz.filter(&principal, folders)?;
    Ok(Json(List { revision, items }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateFolderRequest {
    /// 화면 이름(1~64자, `/` 없음). 형제 사이에서 대소문자 무시로 겹치면 409 `name_taken`
    pub name: String,
    pub kind: FolderKind,
    /// 부모 폴더 id. 없으면 그 트리의 루트 바로 아래다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub parent: Option<Uuid>,
    /// 하위 트리 할당량(VM 폴더만). 부모(없으면 루트)의 `inventory.quota`가 있어야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub quota: Option<FolderQuota>,
}

/// 폴더를 만든다. 부모(없으면 루트)의 `inventory.folder`가 있어야 한다. 부모가 없거나 다른 트리면 404, 깊이 16을 넘으면
/// 409 `folder_too_deep`, 형제 이름이 겹치면 409 `name_taken`.
#[utoipa::path(post, path = "/folders", tag = "folders",
    request_body = CreateFolderRequest,
    responses((status = 201, body = Resource<FolderSpec, FolderStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_folder(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateFolderRequest>,
) -> Result<(StatusCode, Json<Folder>), ApiError> {
    require_feature(&state.cluster, FOLDERS)?;
    let id = Uuid::now_v7();
    let spec = FolderSpec {
        name: req.name,
        kind: req.kind,
        parent: req.parent,
        quota: req.quota.filter(|q| !q.is_empty()),
    };
    require_quota_feature(&state, spec.quota.as_ref())?;
    spec.validate(id)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    require_at(
        &state,
        &principal,
        Privilege::InventoryFolder,
        parent_target(spec.parent),
    )?;
    if spec.quota.is_some() {
        require_at(
            &state,
            &principal,
            Privilege::InventoryQuota,
            parent_target(spec.parent),
        )?;
    }
    if state.cluster.list::<FolderSpec>()?.1.len() >= MAX_FOLDERS {
        return Err(ApiError::conflict(
            "too_many_folders",
            format!("a cluster can have at most {MAX_FOLDERS} folders"),
        ));
    }
    let mut folder: Folder = Resource::new(id.to_string(), spec);
    folder.id = id;
    folder.revision = state.cluster.create(&folder).await?;
    audit.detail(format!(
        "folder create {}",
        folder_path(&state.cluster, folder.id)
    ));
    Ok((StatusCode::CREATED, Json(folder)))
}

#[utoipa::path(get, path = "/folders/{id}", tag = "folders",
    params(("id" = String, Path, description = "folder id")),
    responses((status = 200, body = Resource<FolderSpec, FolderStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_folder(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<Folder>, ApiError> {
    Ok(Json(lookup::<FolderSpec>(&state.cluster, &id)?))
}

/// 없는 칸은 그대로, `null`은 지우기(할당량)
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FolderPatch {
    /// 새 이름(그 폴더의 `inventory.folder`)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// 새 부모 폴더 id, `null`은 트리의 루트(그 폴더와 새 부모 쪽의 `inventory.move`)
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<String>, format = Uuid)]
    pub parent: Option<Option<Uuid>>,
    /// 새 할당량, `null`은 지우기(부모 쪽의 `inventory.quota`)
    #[serde(default, deserialize_with = "present")]
    #[schema(value_type = Option<FolderQuota>)]
    pub quota: Option<Option<FolderQuota>>,
}

/// 폴더의 이름·부모·할당량을 바꾼다. 이름은 그 폴더의 `inventory.folder`, 부모는 그 폴더와 새 부모(없으면 루트)의
/// `inventory.move`, 할당량은 부모(없으면 루트)의 `inventory.quota`(팀 관리자가 자기 상한을 올리지 못하게)가 있어야
/// 한다. 순환은 409 `folder_cycle`, 깊이 16을 넘으면 409 `folder_too_deep`, 형제 이름이 겹치면 409 `name_taken`.
#[utoipa::path(patch, path = "/folders/{id}", tag = "folders",
    params(("id" = String, Path, description = "folder id"), ("If-Match" = Option<u64>, Header)),
    request_body = FolderPatch,
    responses((status = 200, body = Resource<FolderSpec, FolderStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_folder(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(req): Json<FolderPatch>,
) -> Result<Json<Folder>, ApiError> {
    require_feature(&state.cluster, FOLDERS)?;
    let folder = lookup::<FolderSpec>(&state.cluster, &id)?;
    let mut spec = folder.spec.clone();
    let mut changes = Vec::new();
    if let Some(name) = req.name.filter(|n| *n != folder.spec.name) {
        require_at(
            &state,
            &principal,
            Privilege::InventoryFolder,
            AuthzTarget::Folder(folder.id),
        )?;
        changes.push(format!("rename {:?}", name));
        spec.name = name;
    }
    if let Some(parent) = req.parent.filter(|p| *p != folder.spec.parent) {
        require_at(
            &state,
            &principal,
            Privilege::InventoryMove,
            AuthzTarget::Folder(folder.id),
        )?;
        require_at(
            &state,
            &principal,
            Privilege::InventoryMove,
            parent_target(parent),
        )?;
        require_move_inside(&state, &principal, &folder, parent)?;
        changes.push(format!(
            "move -> {}",
            parent.map_or_else(|| "/".to_string(), |p| folder_path(&state.cluster, p))
        ));
        spec.parent = parent;
    }
    if let Some(quota) = req.quota {
        let quota = quota.filter(|q| !q.is_empty());
        if quota != folder.spec.quota {
            require_quota_feature(&state, quota.as_ref())?;
            // 옮기면서 바꾸면 새 부모 쪽에서 본다.
            require_at(
                &state,
                &principal,
                Privilege::InventoryQuota,
                parent_target(spec.parent),
            )?;
            changes.push("quota".into());
            spec.quota = quota;
        }
    }
    if spec == folder.spec {
        return Ok(Json(folder));
    }
    // 하위 트리를 할당량 있는 폴더 아래로 옮기면 그 사용량이 더해진다.
    if spec.parent != folder.spec.parent {
        crate::quota::precheck(
            &state.cluster,
            crate::quota::Change::Folder {
                id: folder.id,
                parent: spec.parent,
            },
        )?;
    }
    spec.validate(folder.id)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    state
        .cluster
        .update_spec(folder.id, if_match(&headers)?, folder.labels.clone(), &spec)
        .await?;
    audit.detail(format!(
        "folder {} {}",
        changes.join(", "),
        folder_path(&state.cluster, folder.id)
    ));
    Ok(Json(lookup::<FolderSpec>(
        &state.cluster,
        &folder.id.to_string(),
    )?))
}

/// 빈 폴더를 지운다(그 폴더의 `inventory.folder`). 자식 폴더나 리소스가 있으면 409 `folder_not_empty`.
#[utoipa::path(delete, path = "/folders/{id}", tag = "folders",
    params(("id" = String, Path, description = "folder id"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_folder(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_feature(&state.cluster, FOLDERS)?;
    let folder = lookup::<FolderSpec>(&state.cluster, &id)?;
    let path = folder_path(&state.cluster, folder.id);
    state
        .cluster
        .delete::<FolderSpec>(folder.id, if_match(&headers)?)
        .await?;
    crate::permissions::drop_orphans(&state.cluster).await;
    audit.detail(format!("folder delete {path}"));
    Ok(StatusCode::NO_CONTENT)
}

/// 옮길 리소스 하나
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberRef {
    /// `vm`(템플릿 포함), `network`, `storage-pool`
    #[schema(value_type = String)]
    pub kind: Kind,
    /// 이름이나 id
    pub key: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveMembersRequest {
    pub items: Vec<MemberRef>,
}

/// 항목 하나의 결과
#[derive(Debug, Serialize, ToSchema)]
pub struct MoveResult {
    #[schema(value_type = String)]
    pub kind: Kind,
    pub key: String,
    /// 옮겼거나 이미 그 폴더에 있었다.
    pub ok: bool,
    /// 실패한 이유(`code`·`message`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<crate::error::ErrorBody>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MoveMembersResponse {
    pub items: Vec<MoveResult>,
}

/// 리소스 `kind`의 `key`를 폴더 `dest`로 옮긴다(그 리소스의 `inventory.move`). 같은 트리가 아니면 400.
async fn move_one(
    state: &AppState,
    principal: &Principal,
    dest: Option<&Folder>,
    item: &MemberRef,
) -> Result<String, ApiError> {
    let tree = FolderKind::of(item.kind).ok_or_else(|| {
        ApiError::bad_request(format!("{} cannot be put in folders", item.kind.as_str()))
    })?;
    if let Some(dest) = dest
        && dest.spec.kind != tree
    {
        return Err(ApiError::bad_request(format!(
            "{} cannot go in a {} folder",
            item.kind.as_str(),
            dest.spec.kind.as_str()
        )));
    }
    let to = dest.map(|f| f.id);
    macro_rules! move_resource {
        ($spec:ty, $check:expr) => {{
            for _ in 0..10 {
                let r = lookup::<$spec>(&state.cluster, &item.key)?;
                state
                    .authz
                    .require(principal, Privilege::InventoryMove, &r)?;
                if r.spec.folder == to {
                    return Ok(r.name);
                }
                let mut spec = r.spec.clone();
                spec.folder = to;
                $check(&r, &spec)?;
                match state
                    .cluster
                    .update_spec(r.id, Some(r.revision), r.labels.clone(), &spec)
                    .await
                {
                    Ok(_) => return Ok(r.name),
                    Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
                    Err(e) => return Err(e.into()),
                }
            }
            Err(ApiError::conflict(
                "busy",
                format!(
                    "{} {:?} kept changing; try again",
                    item.kind.as_str(),
                    item.key
                ),
            ))
        }};
    }
    match item.kind {
        Kind::Vm => {
            // VM 스펙 쓰기와 엇갈리지 않게 한다(이 노드의 검사·쓰기 잠금).
            let _lock = state.vm_lock.lock().await;
            // 할당량 있는 폴더로 옮기면 그 VM의 몫이 더해진다. 전용 CPU·hugepages VM은 복제·배포처럼 옮길 폴더에
            // `vm.dedicated`가 있어야 한다(계획 64 리뷰 2 N6).
            move_resource!(VmSpec, |r: &opl_types::vm::Vm, spec: &VmSpec| {
                crate::numa::require_privilege(
                    state,
                    principal,
                    opl_types::numa::uses_host_resources(spec),
                    spec.folder,
                )?;
                crate::quota::precheck_vm(&state.cluster, Some((r.id, &r.spec)), spec)
            })
        }
        Kind::Network => {
            move_resource!(NetworkSpec, |_: &_, _: &NetworkSpec| Ok::<_, ApiError>(()))
        }
        Kind::StoragePool => {
            move_resource!(StoragePoolSpec, |_: &_, _: &StoragePoolSpec| Ok::<
                _,
                ApiError,
            >(
                ()
            ))
        }
        Kind::Router | Kind::FloatingIp => Err(crate::sdn::routing_not_yet(
            &state.cluster,
            "routers and floating IPs",
        )),
        _ => unreachable!("FolderKind::of"),
    }
}

/// 리소스를 이 폴더로 옮긴다(일괄, 항목마다 결과). 대상 폴더의 `inventory.move`와 항목마다 그 리소스(원본 쪽)의
/// `inventory.move`가 있어야 한다. 폴더 id 대신 `root`면 그 트리의 루트로 옮긴다(루트의 `inventory.move`). 항목의
/// 실패(볼 수 없으면 404, 권한 403, 다른 트리 400)는 그 항목의 `error`에 담고 응답은 200이다.
#[utoipa::path(post, path = "/folders/{id}/members", tag = "folders",
    params(("id" = String, Path, description = "folder id, or root")),
    request_body = MoveMembersRequest,
    responses((status = 200, body = MoveMembersResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn move_members(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
    Json(req): Json<MoveMembersRequest>,
) -> Result<Json<MoveMembersResponse>, ApiError> {
    require_feature(&state.cluster, FOLDERS)?;
    let dest = if id == "root" {
        state
            .authz
            .require_root(&principal, Privilege::InventoryMove)?;
        None
    } else {
        // 대상 폴더의 `inventory.move`는 경로표가 봤다.
        Some(lookup::<FolderSpec>(&state.cluster, &id)?)
    };
    let to = dest
        .as_ref()
        .map_or_else(|| "/".to_string(), |f| folder_path(&state.cluster, f.id));
    let mut items = Vec::with_capacity(req.items.len());
    let mut moved = Vec::new();
    for item in req.items {
        match move_one(&state, &principal, dest.as_ref(), &item).await {
            Ok(name) => {
                moved.push(format!("{} {name}", item.kind.as_str()));
                items.push(MoveResult {
                    kind: item.kind,
                    key: item.key,
                    ok: true,
                    error: None,
                });
            }
            Err(e) => items.push(MoveResult {
                kind: item.kind,
                key: item.key,
                ok: false,
                error: Some(crate::error::ErrorBody {
                    code: e.code.to_string(),
                    message: e.message,
                    details: e.details,
                }),
            }),
        }
    }
    if !moved.is_empty() {
        audit.detail(format!("folder move {} -> {to}", moved.join(", ")));
    }
    Ok(Json(MoveMembersResponse { items }))
}
