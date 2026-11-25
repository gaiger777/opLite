//! 사용자 정의 역할·그룹·grant·유효 권한 API(계획 62 설계 §6·§13). 쓰기는 모든 노드가 `folders`를 알 때만 받는다
//! (409 `cluster_upgrading`). 역할·그룹·루트 grant 쓰기는 `cluster.admin`이고, 폴더·객체 grant는 대상의
//! `permission.modify`와 "자기 권한 안에서"(역할의 권한이 그 대상에서 자기 유효 권한의 부분집합)를 요구한다.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use orbivisor_cluster::Cluster;
use orbivisor_types::auth::{Role, UserSpec};
use orbivisor_types::authz::{
    AccessRoleSpec, AccessRoleStatus, GrantSpec, GrantStatus, GrantTarget, GroupSpec, GroupStatus,
    MAX_GRANTS, PrincipalRef, PrivSet, Privilege, RoleRef, builtin_privileges, validate_role_name,
};
use orbivisor_types::authz_eval::{AuthzTarget, chain_of, groups_of, reaches};
use orbivisor_types::folder::FolderSpec;
use orbivisor_types::network::NetworkSpec;
use orbivisor_types::node::{FILE_RESTORE, FOLDERS, VM_EXPORT};
use orbivisor_types::resource::{Kind, Resource};
use orbivisor_types::storage::StoragePoolSpec;
use orbivisor_types::vm::VmSpec;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz::{AuthzObject, EffectiveIndex, Found, Owned, Subject};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, List, UpdateRequest, check_name, if_match, lookup};
use crate::state::AppState;

pub type AccessRole = Resource<AccessRoleSpec, AccessRoleStatus>;
pub type Group = Resource<GroupSpec, GroupStatus>;
pub type Grant = Resource<GrantSpec, GrantStatus>;

/// 내장 역할(저장하지 않는다)
const BUILTIN: [Role; 3] = [Role::Admin, Role::Operator, Role::Viewer];

fn names(set: PrivSet) -> Vec<String> {
    set.names().into_iter().map(str::to_string).collect()
}

fn bad(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string())
}

/// 역할·토큰 범위의 권한 이름에 기능이 드는 권한이 있으면 그 기능을 검증보다 먼저 본다(409 `cluster_upgrading`):
/// `backup.file_restore`(계획 69)는 옛 노드가 이름을 버려 권한 판단이 노드마다 달라지므로 모든 노드가 `file-restore`를
/// 알 때만. `vm.export`(계획 72)도 같은 까닭으로 모든 노드가 `vm-export`를 알 때만.
pub(crate) fn check_privilege_features(
    cluster: &Cluster,
    privileges: &[String],
) -> Result<(), ApiError> {
    for (privilege, feature) in [
        (Privilege::BackupFileRestore, FILE_RESTORE),
        (Privilege::VmExport, VM_EXPORT),
    ] {
        if privileges.iter().any(|p| p == privilege.as_str()) {
            require_feature(cluster, feature)?;
        }
    }
    Ok(())
}

// ---- 역할 ----

/// 역할(내장 또는 사용자 정의)
#[derive(Debug, Serialize, ToSchema)]
pub struct AccessRoleView {
    /// 사용자 정의 역할의 id. 내장 역할은 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub id: Option<Uuid>,
    pub name: String,
    /// 내장 역할(`admin`·`operator`·`viewer`)이면 참이고 바꿀 수 없다.
    pub builtin: bool,
    /// 권한 이름 목록(`vm.power` 등)
    pub privileges: Vec<String>,
    pub description: String,
    /// 사용자 정의 역할의 revision(내장 역할은 0)
    pub revision: u64,
}

fn builtin_view(role: Role) -> AccessRoleView {
    AccessRoleView {
        id: None,
        name: role.as_str().into(),
        builtin: true,
        privileges: names(builtin_privileges(role)),
        description: String::new(),
        revision: 0,
    }
}

impl From<AccessRole> for AccessRoleView {
    fn from(r: AccessRole) -> Self {
        AccessRoleView {
            id: Some(r.id),
            privileges: names(r.spec.privset().0),
            name: r.name,
            builtin: false,
            description: r.spec.description,
            revision: r.revision,
        }
    }
}

/// 내장 역할 셋과 사용자 정의 역할. 로그인한 누구나 본다(grant를 주는 위임자가 고른다).
#[utoipa::path(get, path = "/access-roles", tag = "permissions",
    responses((status = 200, body = List<AccessRoleView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_access_roles(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
) -> Result<Json<List<AccessRoleView>>, ApiError> {
    let (revision, roles) = state.cluster.list::<AccessRoleSpec>()?;
    let mut items: Vec<AccessRoleView> = BUILTIN.into_iter().map(builtin_view).collect();
    items.extend(roles.into_iter().map(AccessRoleView::from));
    Ok(Json(List { revision, items }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAccessRoleRequest {
    pub name: String,
    pub privileges: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// 사용자 정의 역할을 만든다(`cluster.admin`). 이름 `admin`·`operator`·`viewer`·`none`은 400, 모르는 권한 이름은 400.
#[utoipa::path(post, path = "/access-roles", tag = "permissions",
    request_body = CreateAccessRoleRequest,
    responses((status = 201, body = AccessRoleView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_access_role(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateAccessRoleRequest>,
) -> Result<(StatusCode, Json<AccessRoleView>), ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    require_feature(&state.cluster, FOLDERS)?;
    check_privilege_features(&state.cluster, &req.privileges)?;
    check_name(&req.name)?;
    validate_role_name(&req.name).map_err(bad)?;
    let spec = AccessRoleSpec {
        privileges: req.privileges,
        description: req.description,
    };
    spec.validate().map_err(bad)?;
    let mut role: AccessRole = Resource::new(req.name, spec);
    role.revision = state.cluster.create(&role).await?;
    audit.detail(format!("role create {}", role.name));
    Ok((StatusCode::CREATED, Json(role.into())))
}

/// 내장 역할 이름이면 그 역할
fn builtin_named(key: &str) -> Option<Role> {
    BUILTIN.into_iter().find(|r| r.as_str() == key)
}

#[utoipa::path(get, path = "/access-roles/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name (admin, operator and viewer are the built-in roles)")),
    responses((status = 200, body = AccessRoleView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_access_role(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<AccessRoleView>, ApiError> {
    if let Some(role) = builtin_named(&key) {
        return Ok(Json(builtin_view(role)));
    }
    Ok(Json(lookup::<AccessRoleSpec>(&state.cluster, &key)?.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccessRoleRequest {
    pub privileges: Vec<String>,
    #[serde(default)]
    pub description: String,
}

fn builtin_fixed() -> ApiError {
    ApiError::bad_request("built-in roles cannot be changed")
}

/// 권한 목록을 바꾼다(`cluster.admin`). 이 역할을 건 모든 grant가 바로 바뀐다. 내장 역할은 400.
#[utoipa::path(put, path = "/access-roles/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateAccessRoleRequest,
    responses((status = 200, body = AccessRoleView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_access_role(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateAccessRoleRequest>,
) -> Result<Json<AccessRoleView>, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    require_feature(&state.cluster, FOLDERS)?;
    check_privilege_features(&state.cluster, &req.privileges)?;
    if builtin_named(&key).is_some() {
        return Err(builtin_fixed());
    }
    let role = lookup::<AccessRoleSpec>(&state.cluster, &key)?;
    let spec = AccessRoleSpec {
        privileges: req.privileges,
        description: req.description,
    };
    spec.validate().map_err(bad)?;
    state
        .cluster
        .update_spec(role.id, if_match(&headers)?, role.labels, &spec)
        .await?;
    audit.detail(format!("role set {}", role.name));
    Ok(Json(
        lookup::<AccessRoleSpec>(&state.cluster, &role.id.to_string())?.into(),
    ))
}

/// 사용자 정의 역할을 지운다(`cluster.admin`). 이 역할을 건 grant가 있으면 409 `role_in_use`, 내장 역할은 400.
#[utoipa::path(delete, path = "/access-roles/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_access_role(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    if builtin_named(&key).is_some() {
        return Err(builtin_fixed());
    }
    let role = lookup::<AccessRoleSpec>(&state.cluster, &key)?;
    let (_, grants) = state.cluster.list::<GrantSpec>()?;
    let used = grants
        .iter()
        .filter(|g| g.spec.role == RoleRef::Custom { id: role.id })
        .count();
    if used > 0 {
        return Err(ApiError::conflict(
            "role_in_use",
            format!(
                "role {:?} is used by {used} grant(s); remove them first",
                role.name
            ),
        ));
    }
    state
        .cluster
        .delete::<AccessRoleSpec>(role.id, if_match(&headers)?)
        .await?;
    audit.detail(format!("role delete {}", role.name));
    Ok(StatusCode::NO_CONTENT)
}

// ---- 그룹 ----

/// 그룹의 멤버가 모두 있는 사용자인지(400). `stored`(고치기 전 멤버)에 있던 지운 사용자는 오류 대신 빼고 쓴다(리뷰 2 N4 —
/// GET으로 받은 목록을 그대로 되보내도 된다).
fn check_group(state: &AppState, spec: &mut GroupSpec, stored: &[Uuid]) -> Result<(), ApiError> {
    spec.validate().map_err(bad)?;
    let mut gone = HashSet::new();
    for id in spec.member_set() {
        if state.cluster.get::<UserSpec>(id)?.is_none() {
            if !stored.contains(&id) {
                return Err(ApiError::bad_request(format!("user {id} not found")));
            }
            gone.insert(id);
        }
    }
    spec.members.retain(|id| !gone.contains(id));
    Ok(())
}

/// 지운 주체·대상을 가리키는 grant와 그룹에 남은 지운 사용자(리뷰 2 N4·N5). 사용자·폴더·VM·네트워크·풀을 지운 뒤
/// 그 핸들러가 부르고, 놓친 것(실패·옛 상태)은 리더의 정리 작업이 tick마다 부른다. 실패는 기록만 한다(다음에 다시).
/// 색인은 이런 grant를 이미 무시하므로(없는 사용자·대상) 권한에는 영향이 없고, 쌓이지 않게 지우는 것이다.
pub(crate) async fn drop_orphans(cluster: &Cluster) -> usize {
    // grant·그룹을 먼저 읽는다: grant는 주체·대상이 있을 때만 만들어지므로 뒤에 읽은 목록에 없으면 그사이 지운 것이다
    // (반대 순서면 막 만든 사용자의 새 grant를 고아로 잘못 볼 수 있다).
    let found = (|| {
        let grants = cluster.list::<GrantSpec>()?.1;
        let groups = cluster.list::<GroupSpec>()?.1;
        let users: HashSet<Uuid> = cluster.list::<UserSpec>()?.1.iter().map(|u| u.id).collect();
        let mut objects: HashSet<(Kind, Uuid)> = HashSet::new();
        objects.extend(
            cluster
                .list::<FolderSpec>()?
                .1
                .iter()
                .map(|f| (Kind::Folder, f.id)),
        );
        objects.extend(cluster.list::<VmSpec>()?.1.iter().map(|v| (Kind::Vm, v.id)));
        objects.extend(
            cluster
                .list::<NetworkSpec>()?
                .1
                .iter()
                .map(|n| (Kind::Network, n.id)),
        );
        objects.extend(
            cluster
                .list::<StoragePoolSpec>()?
                .1
                .iter()
                .map(|p| (Kind::StoragePool, p.id)),
        );
        objects.extend(
            cluster
                .list::<orbivisor_types::router::RouterSpec>()?
                .1
                .iter()
                .map(|r| (Kind::Router, r.id)),
        );
        objects.extend(
            cluster
                .list::<orbivisor_types::floating_ip::FloatingIpSpec>()?
                .1
                .iter()
                .map(|f| (Kind::FloatingIp, f.id)),
        );
        objects.extend(
            cluster
                .list::<orbivisor_types::load_balancer::LoadBalancerSpec>()?
                .1
                .iter()
                .map(|l| (Kind::LoadBalancer, l.id)),
        );
        objects.extend(
            cluster
                .list::<orbivisor_types::network_migration::NetworkMigrationSpec>()?
                .1
                .iter()
                .map(|m| (Kind::NetworkMigration, m.id)),
        );
        Ok::<_, orbivisor_cluster::ClusterError>((users, groups, objects, grants))
    })();
    let Ok((users, groups, objects, grants)) = found else {
        return 0;
    };
    let group_ids: HashSet<Uuid> = groups.iter().map(|g| g.id).collect();
    let mut dropped = 0;
    for grant in grants
        .iter()
        .filter(|g| orphan_grant(&g.spec, &users, &group_ids, &objects))
        .take(ORPHANS_PER_PASS)
    {
        match cluster
            .delete::<GrantSpec>(grant.id, Some(grant.revision))
            .await
        {
            Ok(_) => dropped += 1,
            Err(e) => tracing::info!(grant = %grant.id, error = %e, "orphan grant not deleted"),
        }
    }
    for group in groups
        .iter()
        .filter(|g| g.spec.members.iter().any(|m| !users.contains(m)))
    {
        let mut spec = group.spec.clone();
        spec.members.retain(|m| users.contains(m));
        match cluster
            .update_spec(group.id, Some(group.revision), group.labels.clone(), &spec)
            .await
        {
            Ok(_) => dropped += 1,
            Err(e) => {
                tracing::info!(group = %group.name, error = %e, "deleted members not removed")
            }
        }
    }
    dropped
}

/// 한 번에 지우는 고아 grant 수. 나머지는 다음 tick에 지운다.
const ORPHANS_PER_PASS: usize = 200;

/// 주체(사용자·그룹)나 대상(폴더·VM·네트워크·풀)이 없는 grant. 루트 grant의 대상은 늘 있다.
fn orphan_grant(
    grant: &GrantSpec,
    users: &HashSet<Uuid>,
    groups: &HashSet<Uuid>,
    objects: &HashSet<(Kind, Uuid)>,
) -> bool {
    let principal = match grant.principal {
        PrincipalRef::User { id } => users.contains(&id),
        PrincipalRef::Group { id } => groups.contains(&id),
    };
    let target = match grant.target {
        GrantTarget::Root => true,
        GrantTarget::Folder { id } => objects.contains(&(Kind::Folder, id)),
        GrantTarget::Object { kind, id } => {
            orbivisor_types::folder::FolderKind::of(kind).is_none() || objects.contains(&(kind, id))
        }
    };
    !(principal && target)
}

#[utoipa::path(get, path = "/groups", tag = "permissions",
    responses((status = 200, body = List<Resource<GroupSpec, GroupStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_groups(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<Group>>, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    let (revision, items) = state.cluster.list::<GroupSpec>()?;
    Ok(Json(List { revision, items }))
}

/// 로컬 그룹을 만든다(`cluster.admin`). `members`는 사용자 id, `external`은 영역의 디렉터리 그룹이다(그 영역 사용자가
/// 로그인 때 그 그룹에 있었으면 든다).
#[utoipa::path(post, path = "/groups", tag = "permissions",
    request_body = CreateRequest<GroupSpec>,
    responses((status = 201, body = Resource<GroupSpec, GroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateRequest<GroupSpec>>,
) -> Result<(StatusCode, Json<Group>), ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    require_feature(&state.cluster, FOLDERS)?;
    check_name(&req.name)?;
    let mut spec = req.spec;
    check_group(&state, &mut spec, &[])?;
    let mut group: Group = Resource::new(req.name, spec);
    group.labels = req.labels;
    group.revision = state.cluster.create(&group).await?;
    audit.detail(format!("group create {}", group.name));
    Ok((StatusCode::CREATED, Json(group)))
}

#[utoipa::path(get, path = "/groups/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<GroupSpec, GroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Group>, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    Ok(Json(lookup::<GroupSpec>(&state.cluster, &key)?))
}

#[utoipa::path(put, path = "/groups/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<GroupSpec>,
    responses((status = 200, body = Resource<GroupSpec, GroupStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<GroupSpec>>,
) -> Result<Json<Group>, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    require_feature(&state.cluster, FOLDERS)?;
    let group = lookup::<GroupSpec>(&state.cluster, &key)?;
    let mut spec = req.spec;
    check_group(&state, &mut spec, &group.spec.members)?;
    state
        .cluster
        .update_spec(group.id, if_match(&headers)?, req.labels, &spec)
        .await?;
    audit.detail(format!("group set {}", group.name));
    Ok(Json(lookup::<GroupSpec>(
        &state.cluster,
        &group.id.to_string(),
    )?))
}

/// 그룹을 지운다(`cluster.admin`). 이 그룹에 건 grant가 있으면 409 `group_in_use`.
#[utoipa::path(delete, path = "/groups/{key}", tag = "permissions",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_group(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state
        .authz
        .require_root(&principal, Privilege::ClusterAdmin)?;
    let group = lookup::<GroupSpec>(&state.cluster, &key)?;
    let (_, grants) = state.cluster.list::<GrantSpec>()?;
    let used = grants
        .iter()
        .filter(|g| g.spec.principal == PrincipalRef::Group { id: group.id })
        .count();
    if used > 0 {
        return Err(ApiError::conflict(
            "group_in_use",
            format!(
                "group {:?} has {used} grant(s); remove them first",
                group.name
            ),
        ));
    }
    state
        .cluster
        .delete::<GroupSpec>(group.id, if_match(&headers)?)
        .await?;
    audit.detail(format!("group delete {}", group.name));
    Ok(StatusCode::NO_CONTENT)
}

// ---- grant ----

/// grant 대상의 평가 대상. 객체는 지금 폴더를 읽는다. 없으면 None
fn grant_target(state: &AppState, target: &GrantTarget) -> Result<Option<AuthzTarget>, ApiError> {
    Ok(match *target {
        GrantTarget::Root => Some(AuthzTarget::Root),
        GrantTarget::Folder { id } => state
            .cluster
            .get::<FolderSpec>(id)?
            .map(|_| AuthzTarget::Folder(id)),
        GrantTarget::Object { kind, id } => {
            let folder = match kind {
                Kind::Vm => state.cluster.get::<VmSpec>(id)?.map(|v| v.spec.folder),
                Kind::Network => state.cluster.get::<NetworkSpec>(id)?.map(|n| n.spec.folder),
                Kind::StoragePool => state
                    .cluster
                    .get::<StoragePoolSpec>(id)?
                    .map(|p| p.spec.folder),
                Kind::Router => state
                    .cluster
                    .get::<orbivisor_types::router::RouterSpec>(id)?
                    .map(|r| r.spec.folder),
                Kind::FloatingIp => state
                    .cluster
                    .get::<orbivisor_types::floating_ip::FloatingIpSpec>(id)?
                    .map(|f| f.spec.folder),
                Kind::LoadBalancer => state
                    .cluster
                    .get::<orbivisor_types::load_balancer::LoadBalancerSpec>(id)?
                    .map(|l| l.spec.folder),
                Kind::NetworkMigration => state
                    .cluster
                    .get::<orbivisor_types::network_migration::NetworkMigrationSpec>(id)?
                    .map(|m| m.spec.folder),
                _ => None,
            };
            folder.map(|folder| AuthzTarget::Object { kind, id, folder })
        }
    })
}

/// 읽을 때의 대상: 없는 객체·폴더에 남은 grant는 루트 관리자만 본다.
fn grant_target_or_root(state: &AppState, target: &GrantTarget) -> AuthzTarget {
    grant_target(state, target)
        .ok()
        .flatten()
        .unwrap_or(AuthzTarget::Root)
}

/// 주체가 이 grant를 볼 수 있는지: `cluster.admin`이거나, 자기(또는 자기 그룹)의 grant이거나, 대상에서
/// `permission.modify`가 있다.
fn grant_visible(
    state: &AppState,
    index: &EffectiveIndex,
    me: Uuid,
    my_groups: &HashSet<Uuid>,
    grant: &GrantSpec,
) -> bool {
    if index
        .get(&AuthzTarget::Root)
        .contains(Privilege::ClusterAdmin)
    {
        return true;
    }
    let mine = match grant.principal {
        PrincipalRef::User { id } => id == me,
        PrincipalRef::Group { id } => my_groups.contains(&id),
    };
    mine || index
        .get(&grant_target_or_root(state, &grant.target))
        .contains(Privilege::PermissionModify)
}

fn my_groups(state: &AppState, me: Uuid) -> Result<HashSet<Uuid>, ApiError> {
    state.authz.with_snapshot(|snap| groups_of(snap, me))
}

/// `/watch`의 grant 이벤트 거르기(JSON 스펙)
pub(crate) fn grant_visible_raw(
    state: &AppState,
    principal: &Principal,
    index: &EffectiveIndex,
    resource: &orbivisor_cluster::types::RawResource,
) -> bool {
    let Ok(grant) = serde_json::from_value::<GrantSpec>(resource.spec.clone()) else {
        return false;
    };
    let groups = my_groups(state, principal.user).unwrap_or_default();
    grant_visible(state, index, principal.user, &groups, &grant)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct GrantQuery {
    /// 대상: `root`, `folder/<id>`, `vm/<id>`, `network/<id>`, `storage-pool/<id>`, `router/<id>`, `floating-ip/<id>`
    pub target: Option<String>,
    /// 주체: `user/<id>`, `group/<id>`
    pub principal: Option<String>,
}

fn parse_grant_target(s: &str) -> Result<GrantTarget, ApiError> {
    if s == "root" {
        return Ok(GrantTarget::Root);
    }
    let (kind, id) = s
        .split_once('/')
        .ok_or_else(|| ApiError::bad_request(format!("bad target {s:?}")))?;
    let id = Uuid::try_parse(id).map_err(|_| ApiError::bad_request(format!("bad target {s:?}")))?;
    Ok(match kind {
        "folder" => GrantTarget::Folder { id },
        "vm" => GrantTarget::Object { kind: Kind::Vm, id },
        "network" => GrantTarget::Object {
            kind: Kind::Network,
            id,
        },
        "storage-pool" => GrantTarget::Object {
            kind: Kind::StoragePool,
            id,
        },
        "router" => GrantTarget::Object {
            kind: Kind::Router,
            id,
        },
        "floating-ip" => GrantTarget::Object {
            kind: Kind::FloatingIp,
            id,
        },
        "load-balancer" => GrantTarget::Object {
            kind: Kind::LoadBalancer,
            id,
        },
        "network-migration" => GrantTarget::Object {
            kind: Kind::NetworkMigration,
            id,
        },
        _ => return Err(ApiError::bad_request(format!("bad target {s:?}"))),
    })
}

fn parse_principal(s: &str) -> Result<PrincipalRef, ApiError> {
    let bad = || ApiError::bad_request(format!("bad principal {s:?}"));
    let (kind, id) = s.split_once('/').ok_or_else(bad)?;
    let id = Uuid::try_parse(id).map_err(|_| bad())?;
    match kind {
        "user" => Ok(PrincipalRef::User { id }),
        "group" => Ok(PrincipalRef::Group { id }),
        _ => Err(bad()),
    }
}

/// 볼 수 있는 grant(`cluster.admin`은 전부, 그 밖에는 자기·자기 그룹의 것과 `permission.modify`가 있는 대상의 것).
/// `target`·`principal`로 좁힌다.
#[utoipa::path(get, path = "/grants", tag = "permissions",
    params(GrantQuery),
    responses((status = 200, body = List<Resource<GrantSpec, GrantStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_grants(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<GrantQuery>,
) -> Result<Json<List<Grant>>, ApiError> {
    let target = q.target.as_deref().map(parse_grant_target).transpose()?;
    let who = q.principal.as_deref().map(parse_principal).transpose()?;
    let (revision, grants) = state.cluster.list::<GrantSpec>()?;
    let index = state.authz.index(Subject::from(&principal))?;
    let groups = my_groups(&state, principal.user)?;
    let items = grants
        .into_iter()
        .filter(|g| target.is_none_or(|t| g.spec.target == t))
        .filter(|g| who.is_none_or(|p| g.spec.principal == p))
        .filter(|g| grant_visible(&state, &index, principal.user, &groups, &g.spec))
        .collect();
    Ok(Json(List { revision, items }))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateGrantRequest {
    pub principal: PrincipalRef,
    pub role: RoleRef,
    pub target: GrantTarget,
    /// 하위 폴더와 그 안의 객체로 내려가는지(기본 참)
    #[serde(default = "yes")]
    #[schema(default = true)]
    pub propagate: bool,
}

fn yes() -> bool {
    true
}

/// 역할의 권한(없는 사용자 정의 역할은 400)
pub(crate) fn role_set(state: &AppState, role: &RoleRef) -> Result<(PrivSet, String), ApiError> {
    match role {
        RoleRef::Builtin { role } => Ok((builtin_privileges(*role), role.as_str().into())),
        RoleRef::Custom { id } => {
            let r = state
                .cluster
                .get::<AccessRoleSpec>(*id)?
                .ok_or_else(|| ApiError::bad_request(format!("access role {id} not found")))?;
            Ok((r.spec.privset().0, r.name))
        }
    }
}

/// 감사·메시지에 쓰는 주체 이름(`user:alice`, `group:team-a`)
fn principal_name(state: &AppState, p: &PrincipalRef) -> Result<String, ApiError> {
    Ok(match p {
        PrincipalRef::User { id } => format!(
            "user:{}",
            state
                .cluster
                .get::<UserSpec>(*id)?
                .ok_or_else(|| ApiError::bad_request(format!("user {id} not found")))?
                .name
        ),
        PrincipalRef::Group { id } => format!(
            "group:{}",
            state
                .cluster
                .get::<GroupSpec>(*id)?
                .ok_or_else(|| ApiError::bad_request(format!("group {id} not found")))?
                .name
        ),
    })
}

/// 감사에 쓰는 대상 이름(`/`, `/teamA/prod`, `vm web`)
fn target_name(state: &AppState, t: &GrantTarget) -> String {
    match *t {
        GrantTarget::Root => "/".into(),
        GrantTarget::Folder { id } => crate::folders::folder_path(&state.cluster, id),
        GrantTarget::Object { kind, id } => {
            let name = match kind {
                Kind::Vm => state
                    .cluster
                    .get::<VmSpec>(id)
                    .ok()
                    .flatten()
                    .map(|v| v.name),
                Kind::Network => state
                    .cluster
                    .get::<NetworkSpec>(id)
                    .ok()
                    .flatten()
                    .map(|n| n.name),
                Kind::StoragePool => state
                    .cluster
                    .get::<StoragePoolSpec>(id)
                    .ok()
                    .flatten()
                    .map(|p| p.name),
                _ => None,
            };
            format!(
                "{} {}",
                kind.as_str(),
                name.unwrap_or_else(|| id.to_string())
            )
        }
    }
}

/// grant를 주거나 뺄 수 있는지(설계 §6.5, 리뷰 1): grant가 닿는 모든 곳에서 역할의 권한과 `permission.modify`(루트
/// grant는 `cluster.admin`)가 자기 유효 권한 안이어야 한다(403 `cannot grant privileges you do not hold: …`). 닿는 곳은
/// 대상 자체이고, propagate면 그 아래 모두다. 아래 모두에 닿는 자기 권한은 대상이나 조상의 propagate grant뿐이라서
/// 폴더 안 객체 층(루트면 루트의 객체 층)으로 본다. 그래서 propagate가 아닌 폴더 grant로 받은 권한을 propagate grant로
/// 넓히지 못한다. 대상이 없거나 볼 수 없으면 404다.
pub(crate) fn check_delegation(
    state: &AppState,
    principal: &Principal,
    target: &GrantTarget,
    propagate: bool,
    privileges: PrivSet,
) -> Result<(), ApiError> {
    // (확인할 대상, 그곳에서 있어야 하는 권한, 메시지에 붙일 곳)
    let mut layers: Vec<(AuthzTarget, PrivSet, &str)> = Vec::new();
    match target {
        GrantTarget::Root => {
            state
                .authz
                .require_root(principal, Privilege::ClusterAdmin)?;
            layers.push((AuthzTarget::Root, privileges, ""));
            if propagate {
                layers.push((contents_of(None, Kind::Vm), privileges, " everywhere below"));
            }
        }
        GrantTarget::Folder { id } => {
            let missing = format!("folder {id} not found");
            let folder = state
                .cluster
                .get::<FolderSpec>(*id)?
                .ok_or_else(|| ApiError::not_found(missing.clone()))?;
            let found = Found {
                owned: Owned::object(AuthzTarget::Folder(*id)),
                missing,
            };
            state
                .authz
                .require(principal, Privilege::PermissionModify, &found)?;
            let needed = privileges | PrivSet::from(Privilege::PermissionModify);
            layers.push((AuthzTarget::Folder(*id), needed, ""));
            if propagate {
                let kind = folder.spec.kind.member_kinds()[0];
                layers.push((contents_of(Some(*id), kind), needed, " everywhere inside"));
            }
        }
        GrantTarget::Object { kind, id } => {
            let missing = format!("{} {id} not found", kind.as_str());
            let Some(at) = grant_target(state, target)? else {
                return Err(ApiError::not_found(missing));
            };
            let found = Found {
                owned: Owned::object(at),
                missing,
            };
            state
                .authz
                .require(principal, Privilege::PermissionModify, &found)?;
            layers.push((
                at,
                privileges | PrivSet::from(Privilege::PermissionModify),
                "",
            ));
        }
    }
    for (at, needed, place) in layers {
        let mine = state.authz.effective(principal, &at)?;
        let extra = needed.difference(mine);
        if !extra.is_empty() {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!(
                    "cannot grant privileges you do not hold{place}: {}",
                    extra.names().join(", ")
                ),
            ));
        }
    }
    Ok(())
}

/// 폴더 `folder`(없으면 루트) 안에 놓일 객체 층: 그 폴더나 조상의 propagate grant만 닿는다.
fn contents_of(folder: Option<Uuid>, kind: Kind) -> AuthzTarget {
    AuthzTarget::Object {
        kind,
        id: Uuid::nil(),
        folder,
    }
}

/// grant를 준다. 루트 grant는 `cluster.admin`, 폴더·객체 grant는 대상의 `permission.modify`가 있어야 하고 역할의 권한이
/// 그 대상에서 자기 유효 권한 안이어야 한다(403). 주체·역할이 없으면 400, 대상이 없거나 볼 수 없으면 404.
#[utoipa::path(post, path = "/grants", tag = "permissions",
    request_body = CreateGrantRequest,
    responses((status = 201, body = Resource<GrantSpec, GrantStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_grant(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateGrantRequest>,
) -> Result<(StatusCode, Json<Grant>), ApiError> {
    require_feature(&state.cluster, FOLDERS)?;
    let spec = GrantSpec {
        principal: req.principal,
        role: req.role,
        target: req.target,
        propagate: req.propagate,
    };
    spec.validate().map_err(bad)?;
    let (privileges, role_name) = role_set(&state, &spec.role)?;
    let who = principal_name(&state, &spec.principal)?;
    check_delegation(&state, &principal, &spec.target, spec.propagate, privileges)?;
    if state.cluster.list::<GrantSpec>()?.1.len() >= MAX_GRANTS {
        return Err(ApiError::conflict(
            "too_many_grants",
            format!("a cluster can have at most {MAX_GRANTS} grants"),
        ));
    }
    let id = Uuid::now_v7();
    let mut grant: Grant = Resource::new(id.to_string(), spec);
    grant.id = id;
    grant.revision = state.cluster.create(&grant).await?;
    audit.detail(format!(
        "grant add {who} {role_name} on {}{}",
        target_name(&state, &grant.spec.target),
        if grant.spec.propagate {
            ""
        } else {
            " (no propagate)"
        }
    ));
    Ok((StatusCode::CREATED, Json(grant)))
}

/// 볼 수 있는 grant 하나(아니면 404)
fn visible_grant(state: &AppState, principal: &Principal, key: &str) -> Result<Grant, ApiError> {
    let grant = lookup::<GrantSpec>(&state.cluster, key)?;
    let index = state.authz.index(Subject::from(principal))?;
    let groups = my_groups(state, principal.user)?;
    if grant_visible(state, &index, principal.user, &groups, &grant.spec) {
        Ok(grant)
    } else {
        Err(ApiError::not_found(format!("grant {key:?} not found")))
    }
}

#[utoipa::path(get, path = "/grants/{id}", tag = "permissions",
    params(("id" = String, Path, description = "grant id")),
    responses((status = 200, body = Resource<GrantSpec, GrantStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_grant(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<String>,
) -> Result<Json<Grant>, ApiError> {
    Ok(Json(visible_grant(&state, &principal, &id)?))
}

/// grant를 뺀다. 루트 grant는 `cluster.admin`, 그 밖에는 대상의 `permission.modify`(없는 대상에 남은 grant는 루트
/// 관리자만). 볼 수 없으면 404.
#[utoipa::path(delete, path = "/grants/{id}", tag = "permissions",
    params(("id" = String, Path, description = "grant id")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_grant(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let grant = visible_grant(&state, &principal, &id)?;
    match grant_target(&state, &grant.spec.target)? {
        // 빼기도 주기와 같은 규칙이다(닿는 모든 곳에 그 역할의 권한). 지워진 사용자 정의 역할은 아무것도 주지 않는다.
        Some(_) => check_delegation(
            &state,
            &principal,
            &grant.spec.target,
            grant.spec.propagate,
            role_set(&state, &grant.spec.role).map_or(PrivSet::EMPTY, |(set, _)| set),
        )?,
        None => state
            .authz
            .require_root(&principal, Privilege::ClusterAdmin)?,
    }
    state.cluster.delete::<GrantSpec>(grant.id, None).await?;
    let who = principal_name(&state, &grant.spec.principal).unwrap_or_else(|_| "?".into());
    let role = role_set(&state, &grant.spec.role)
        .map(|(_, n)| n)
        .unwrap_or_else(|_| "?".into());
    audit.detail(format!(
        "grant remove {who} {role} on {}",
        target_name(&state, &grant.spec.target)
    ));
    Ok(StatusCode::NO_CONTENT)
}

// ---- 유효 권한 ----

/// 객체 하나의 권한
#[derive(Debug, Serialize, ToSchema)]
pub struct ObjectPermissions {
    #[schema(value_type = String)]
    pub kind: Kind,
    pub id: Uuid,
    pub privileges: Vec<String>,
}

/// 내 권한(색인 그대로). 객체의 유효 권한 = 그 객체 폴더의 `contents`(없으면 `root_contents`) ∪ `objects`의 그 객체
#[derive(Debug, Serialize, ToSchema)]
pub struct MyPermissions {
    /// 루트에 모든 권한(전역 admin): 아래 표 없이 전부 참
    pub all: bool,
    /// 루트(클러스터 객체)의 권한
    pub root: Vec<String>,
    /// 루트에 있는 객체(폴더 없음)의 권한
    pub root_contents: Vec<String>,
    /// 폴더 자체의 권한(이름 바꾸기·지우기·그 폴더의 grant)
    pub folders: BTreeMap<Uuid, Vec<String>>,
    /// 폴더 안 객체의 권한(하위 폴더 포함)
    pub contents: BTreeMap<Uuid, Vec<String>>,
    /// 객체에 직접 건 grant로 더해지는 권한
    pub objects: Vec<ObjectPermissions>,
}

/// 요청한 주체(토큰이면 범위까지)의 권한. 웹이 메뉴·단추를 숨길 때 쓴다.
#[utoipa::path(get, path = "/permissions/mine", tag = "permissions",
    responses((status = 200, body = MyPermissions), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn my_permissions(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<MyPermissions>, ApiError> {
    let index = state.authz.index(Subject::from(&principal))?;
    let folders: Vec<Uuid> = state
        .cluster
        .list::<FolderSpec>()?
        .1
        .iter()
        .map(|f| f.id)
        .collect();
    let layers = index.layers(&folders);
    let root_contents = index.get(&AuthzTarget::Object {
        kind: Kind::Vm,
        id: Uuid::nil(),
        folder: None,
    });
    let mut objects: Vec<ObjectPermissions> = layers
        .objects
        .into_iter()
        .map(|((kind, id), set)| ObjectPermissions {
            kind,
            id,
            privileges: names(set),
        })
        .collect();
    objects.sort_by_key(|o| (o.kind.as_str(), o.id));
    Ok(Json(MyPermissions {
        all: layers.all,
        root: names(layers.root),
        root_contents: names(root_contents),
        folders: layers
            .folders
            .into_iter()
            .map(|(k, v)| (k, names(v)))
            .collect(),
        contents: layers
            .contents
            .into_iter()
            .map(|(k, v)| (k, names(v)))
            .collect(),
        objects,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct EffectiveQuery {
    /// 사용자(이름이나 id). 없으면 요청한 주체다.
    pub user: Option<String>,
    /// 대상: `root`, `folder/<id>`, `vm/<key>`, `network/<key>`, `storage-pool/<key>`(key는 이름이나 id)
    pub object: String,
}

/// 유효 권한과 그 근거
#[derive(Debug, Serialize, ToSchema)]
pub struct EffectivePermissions {
    pub user: Uuid,
    pub name: String,
    /// 전역 역할(루트에 건 암묵 grant)
    pub role: Role,
    pub object: String,
    pub privileges: Vec<String>,
    /// 이 대상에 닿는 그 사용자(와 그 그룹)의 grant
    pub grants: Vec<Grant>,
}

/// 경로의 대상 문자열을 찾는다(없거나 볼 수 없으면 404).
fn object_target(
    state: &AppState,
    principal: &Principal,
    object: &str,
) -> Result<AuthzTarget, ApiError> {
    if object == "root" {
        return Ok(AuthzTarget::Root);
    }
    let (kind, key) = object
        .split_once('/')
        .ok_or_else(|| ApiError::bad_request(format!("bad object {object:?}")))?;
    let obj = match kind {
        "folder" => crate::authz_routes::Obj::Folder,
        "vm" => crate::authz_routes::Obj::Vm,
        "network" => crate::authz_routes::Obj::Network,
        "storage-pool" => crate::authz_routes::Obj::StoragePool,
        "router" => crate::authz_routes::Obj::Router,
        "floating-ip" => crate::authz_routes::Obj::FloatingIp,
        "load-balancer" => crate::authz_routes::Obj::LoadBalancer,
        "network-migration" => crate::authz_routes::Obj::NetworkMigration,
        _ => return Err(ApiError::bad_request(format!("bad object {object:?}"))),
    };
    let found = crate::authz_routes::resolve(&state.cluster, obj, key)?
        .ok_or_else(|| ApiError::not_found(format!("{kind} {key:?} not found")))?;
    state
        .authz
        .require(principal, found.read_privilege(), &found)?;
    Ok(found.owned.target)
}

/// 사용자의 대상 유효 권한과 근거 grant. 다른 사용자는 그 대상의 `permission.modify`가 있어야 한다(아니면 403). 대상을
/// 볼 수 없으면 404.
#[utoipa::path(get, path = "/permissions/effective", tag = "permissions",
    params(EffectiveQuery),
    responses((status = 200, body = EffectivePermissions), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn effective_permissions(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<EffectiveQuery>,
) -> Result<Json<EffectivePermissions>, ApiError> {
    let target = object_target(&state, &principal, &q.object)?;
    // 남의 것은 사용자를 찾기 전에 권한을 본다(404·403으로 사용자 이름을 알아내지 못하게, 리뷰 1).
    let me = q.user.as_deref().is_none_or(|key| {
        key == principal.name || Uuid::try_parse(key).is_ok_and(|id| id == principal.user)
    });
    if !me {
        state
            .authz
            .require(&principal, Privilege::PermissionModify, &target)?;
    }
    let user = match &q.user {
        None => lookup::<UserSpec>(&state.cluster, &principal.user.to_string())?,
        Some(key) => lookup::<UserSpec>(&state.cluster, key)?,
    };
    let me = user.id == principal.user;
    let (privileges, grants) = state.authz.with_snapshot(|snap| {
        let privileges = if me {
            None
        } else {
            Some(orbivisor_types::authz_eval::effective(
                snap, user.id, &target,
            ))
        };
        let chain = chain_of(snap, &target);
        let groups = groups_of(snap, user.id);
        let ids: Vec<GrantSpec> = snap
            .grants
            .iter()
            .filter(|g| match g.principal {
                PrincipalRef::User { id } => id == user.id,
                PrincipalRef::Group { id } => groups.contains(&id),
            })
            .filter(|g| reaches(&g.target, g.propagate, &target, &chain))
            .cloned()
            .collect();
        (privileges, ids)
    })?;
    // 자기 것은 토큰 범위까지 본 색인 그대로다.
    let privileges = match privileges {
        Some(p) => p,
        None => state.authz.effective(&principal, &target)?,
    };
    let (_, all) = state.cluster.list::<GrantSpec>()?;
    let grants: Vec<Grant> = all
        .into_iter()
        .filter(|g| grants.contains(&g.spec))
        .collect();
    Ok(Json(EffectivePermissions {
        user: user.id,
        name: user.name,
        role: user.spec.role,
        object: q.object,
        privileges: names(privileges),
        grants,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct WhoQuery {
    /// 대상: `root`, `folder/<id>`, `vm/<key>`, `network/<key>`, `storage-pool/<key>`
    pub object: String,
}

/// 전역 역할이 있는 사용자
#[derive(Debug, Serialize, ToSchema)]
pub struct RoleHolder {
    pub id: Uuid,
    pub name: String,
    pub role: Role,
}

/// 대상에 권한이 닿는 것: grant와 전역 역할 사용자
#[derive(Debug, Serialize, ToSchema)]
pub struct WhoPermissions {
    pub object: String,
    /// 이 대상에 직접 건 것과 조상에 propagate로 건 것
    pub grants: Vec<Grant>,
    /// 전역 역할(`none`이 아닌)이 있는 사용자 — 루트에 건 암묵 grant다. 루트 `cluster.admin`이 아니면 비어 있다.
    pub users: Vec<RoleHolder>,
}

/// 대상에 권한이 닿는 grant와 전역 역할 사용자. 대상의 `permission.modify`가 있어야 한다(403, 볼 수 없으면 404).
/// 전역 역할 사용자 목록은 사용자 목록을 읽는 루트 `cluster.admin`에게만 채운다(리뷰 1).
#[utoipa::path(get, path = "/permissions/who", tag = "permissions",
    params(WhoQuery),
    responses((status = 200, body = WhoPermissions), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn who_permissions(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<WhoQuery>,
) -> Result<Json<WhoPermissions>, ApiError> {
    let target = object_target(&state, &principal, &q.object)?;
    state
        .authz
        .require(&principal, Privilege::PermissionModify, &target)?;
    let chain = state.authz.with_snapshot(|snap| chain_of(snap, &target))?;
    let (_, all) = state.cluster.list::<GrantSpec>()?;
    let grants = all
        .into_iter()
        .filter(|g| reaches(&g.spec.target, g.spec.propagate, &target, &chain))
        .collect();
    let admin = state
        .authz
        .allows(&principal, Privilege::ClusterAdmin, AuthzTarget::Root);
    let users = state
        .cluster
        .list::<UserSpec>()?
        .1
        .into_iter()
        .filter(|u| admin && u.spec.role != Role::None)
        .map(|u| RoleHolder {
            id: u.id,
            name: u.name,
            role: u.spec.role,
        })
        .collect();
    Ok(Json(WhoPermissions {
        object: q.object,
        grants,
        users,
    }))
}
