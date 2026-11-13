//! 종류에 상관없는 조회 핸들러와 공통 요청 형식, 풀·네트워크·노드 핸들러.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::IF_MATCH;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError, Typed};
use opl_types::backup_job::{BackupJobSpec, BackupJobStatus};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
use opl_types::node::{Node, NodeRole, NodeSpec, NodeStatus};
use opl_types::placement::{HaGroupSpec, HaGroupStatus, PlacementRuleSpec, PlacementRuleStatus};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::security_group::{SecurityGroupSpec, SecurityGroupStatus};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, StoragePoolStatus, VolumeSpec, VolumeStatus,
};
use opl_types::task::{TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{VmSpec, VmStatus};
use opl_types::vm_import::{VmImportSpec, VmImportStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::check_no_backups;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest<S> {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest<S> {
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub spec: S,
}

/// 목록과 그 목록이 반영하는 revision. `GET /api/v1/watch?since=<revision>`으로 이어서 구독한다.
#[derive(Debug, Serialize, ToSchema)]
pub struct List<T> {
    pub revision: u64,
    pub items: Vec<T>,
}

/// 경로의 `{key}`는 id(UUID)나 이름이다.
pub fn lookup<S: ResourceSpec>(cluster: &Cluster, key: &str) -> Result<Typed<S>, ApiError> {
    let found = match Uuid::try_parse(key) {
        Ok(id) => cluster.get::<S>(id)?,
        Err(_) => cluster.get_by_name::<S>(key)?,
    };
    found.ok_or_else(|| ApiError::not_found(format!("{} {key:?} not found", S::KIND.as_str())))
}

/// `If-Match: <revision>` (따옴표로 감싸도 된다). 없으면 revision을 확인하지 않는다.
pub fn if_match(headers: &HeaderMap) -> Result<Option<u64>, ApiError> {
    let Some(value) = headers.get(IF_MATCH) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.trim().trim_matches('"').parse().ok())
        .map(Some)
        .ok_or_else(|| ApiError::bad_request("If-Match must be a resource revision"))
}

/// 경로에서 id와 이름을 구분할 수 있도록 UUID 모양의 이름은 받지 않는다.
pub fn check_name(name: &str) -> Result<(), ApiError> {
    validate_name(name).map_err(|e| ApiError::bad_request(e.to_string()))?;
    if Uuid::try_parse(name).is_ok() {
        return Err(ApiError::bad_request(format!(
            "invalid name {name:?}: names must not look like an id"
        )));
    }
    Ok(())
}

fn list<S: ResourceSpec>(
    state: &AppState,
    principal: &Principal,
) -> Result<Json<List<Typed<S>>>, ApiError> {
    state.authorize(principal, Action::Read, S::KIND)?;
    let (revision, items) = state.cluster.list::<S>()?;
    Ok(Json(List { revision, items }))
}

fn get<S: ResourceSpec>(
    state: &AppState,
    principal: &Principal,
    key: &str,
) -> Result<Json<Typed<S>>, ApiError> {
    state.authorize(principal, Action::Read, S::KIND)?;
    Ok(Json(lookup::<S>(&state.cluster, key)?))
}

/// 종류별 목록·조회 핸들러. OpenAPI 문서에 종류마다 경로가 따로 필요해 제네릭 핸들러 대신 쓴다.
macro_rules! list_and_get {
    ($list:ident, $get:ident, $spec:ident, $status:ident, $path:literal, $item_path:literal, $tag:literal) => {
        #[utoipa::path(get, path = $path, tag = $tag, responses(
                                    (status = 200, body = List<Resource<$spec, $status>>),
                                    (status = "4XX", body = crate::error::ErrorBody),
                                ))]
        pub async fn $list(
            State(state): State<Arc<AppState>>,
            principal: Principal,
        ) -> Result<Json<List<Resource<$spec, $status>>>, ApiError> {
            list::<$spec>(&state, &principal)
        }

        #[utoipa::path(get, path = $item_path, tag = $tag,
                                    params(("key" = String, Path, description = "id or name")),
                                    responses(
                                        (status = 200, body = Resource<$spec, $status>),
                                        (status = "4XX", body = crate::error::ErrorBody),
                                    ))]
        pub async fn $get(
            State(state): State<Arc<AppState>>,
            principal: Principal,
            Path(key): Path<String>,
        ) -> Result<Json<Resource<$spec, $status>>, ApiError> {
            get::<$spec>(&state, &principal, &key)
        }
    };
}

list_and_get!(
    list_nodes,
    get_node,
    NodeSpec,
    NodeStatus,
    "/nodes",
    "/nodes/{key}",
    "nodes"
);
list_and_get!(
    list_pools,
    get_pool,
    StoragePoolSpec,
    StoragePoolStatus,
    "/storage-pools",
    "/storage-pools/{key}",
    "storage"
);
list_and_get!(
    list_volumes,
    get_volume,
    VolumeSpec,
    VolumeStatus,
    "/volumes",
    "/volumes/{key}",
    "storage"
);
list_and_get!(
    list_vms,
    get_vm,
    VmSpec,
    VmStatus,
    "/vms",
    "/vms/{key}",
    "vms"
);
list_and_get!(
    list_networks,
    get_network,
    NetworkSpec,
    NetworkStatus,
    "/networks",
    "/networks/{key}",
    "networks"
);
list_and_get!(
    list_security_groups,
    get_security_group,
    SecurityGroupSpec,
    SecurityGroupStatus,
    "/security-groups",
    "/security-groups/{key}",
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
list_and_get!(
    list_vm_imports,
    get_vm_import,
    VmImportSpec,
    VmImportStatus,
    "/vm-imports",
    "/vm-imports/{key}",
    "vm-imports"
);
list_and_get!(
    list_ha_groups,
    get_ha_group,
    HaGroupSpec,
    HaGroupStatus,
    "/ha-groups",
    "/ha-groups/{key}",
    "ha"
);
list_and_get!(
    list_placement_rules,
    get_placement_rule,
    PlacementRuleSpec,
    PlacementRuleStatus,
    "/placement-rules",
    "/placement-rules/{key}",
    "ha"
);
list_and_get!(
    list_tasks,
    get_task,
    TaskSpec,
    TaskStatus,
    "/tasks",
    "/tasks/{key}",
    "tasks"
);

#[utoipa::path(post, path = "/storage-pools", tag = "storage",
    request_body = CreateRequest<StoragePoolSpec>,
    responses((status = 201, body = Resource<StoragePoolSpec, StoragePoolStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<serde_json::Value>>,
) -> Result<(StatusCode, Json<StoragePool>), ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    check_name(&req.name)?;
    // `shared`를 적었는지 알아야 하므로 spec은 JSON 값으로 받아 읽는다.
    let shared_given = req.spec.get("shared").is_some();
    let mut spec: StoragePoolSpec = serde_json::from_value(req.spec)
        .map_err(|e| ApiError::bad_request(format!("spec: {e}")))?;
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // NFS·RBD·공유 LVM 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    let always_shared = match spec.backend {
        PoolBackend::Nfs { .. } => Some("nfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::LvmShared { .. } => Some("lvm-shared"),
        PoolBackend::Dir { .. } | PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } => None,
    };
    if let Some(kind) = always_shared {
        if shared_given && !spec.shared {
            return Err(ApiError::bad_request(format!(
                "{kind} pools are always shared"
            )));
        }
        spec.shared = true;
    }
    // LVM-thin·ZFS 풀은 노드 로컬이다.
    if matches!(spec.backend, PoolBackend::LvmThin { .. }) && spec.shared {
        return Err(ApiError::bad_request(
            "lvm-thin pools are node-local; shared must be false",
        ));
    }
    if matches!(spec.backend, PoolBackend::Zfs { .. }) && spec.shared {
        return Err(ApiError::bad_request(
            "zfs pools are node-local; shared must be false",
        ));
    }
    for node in &spec.nodes {
        if state.cluster.get::<NodeSpec>(*node)?.is_none() {
            return Err(ApiError::bad_request(format!("node {node} not found")));
        }
    }
    let mut pool = Resource::new(req.name, spec);
    pool.labels = req.labels;
    pool.revision = state.cluster.create(&pool).await?;
    Ok((StatusCode::CREATED, Json(pool)))
}

/// 볼륨이나 백업이 남아 있으면 409 `pool_in_use`
#[utoipa::path(delete, path = "/storage-pools/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    let pool = lookup::<StoragePoolSpec>(&state.cluster, &key)?;
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    if let Some(v) = volumes.iter().find(|v| v.spec.pool == pool.name) {
        return Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} still has volume {:?}", pool.name, v.name),
        ));
    }
    check_no_backups(&state, &pool)?;
    state
        .cluster
        .delete::<StoragePoolSpec>(pool.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 풀에서 바꿀 수 있는 값: 디렉터리 풀의 `shared`, dir·NFS 풀의 `backups`. 적지 않은 값은 그대로 둔다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub shared: Option<bool>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub backups: Option<bool>,
}

/// 디렉터리 풀의 `shared`(다른 백엔드는 400)와 백업 풀 표시 `backups`(dir·NFS 풀만 켤 수 있다)를
/// 바꾼다. 둘 다 없으면 400이다. 로컬 → 공유는 언제나 된다. 공유 → 로컬은 풀의 볼륨을 쓰는 VM이 그
/// 볼륨을 만든 노드가 아닌 노드에 있거나 끝나지 않은 마이그레이션 Task가 있으면 409 `pool_in_use`다.
/// 백업이 남은 풀의 `backups`를 끄거나 `shared`를 바꾸면 409 `pool_in_use`다. 같은 값이면 쓰지 않고
/// 그대로 200이다.
#[utoipa::path(patch, path = "/storage-pools/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = PoolPatch,
    responses((status = 200, body = Resource<StoragePoolSpec, StoragePoolStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<PoolPatch>,
) -> Result<Json<StoragePool>, ApiError> {
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    if req.shared.is_none() && req.backups.is_none() {
        return Err(ApiError::bad_request("give shared or backups"));
    }
    let expected = if_match(&headers)?;
    // VM 옮기기·마이그레이션 요청의 검사와 엇갈리지 않게 한다.
    let _lock = state.vm_lock.lock().await;
    for _ in 0..10 {
        let pool = lookup::<StoragePoolSpec>(&state.cluster, &key)?;
        if let Some(expected) = expected
            && expected != pool.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: pool.revision,
            })
            .into());
        }
        let fixed = match pool.spec.backend {
            PoolBackend::Dir { .. } => None,
            PoolBackend::Nfs { .. } => Some("nfs pools are always shared"),
            PoolBackend::Rbd { .. } => Some("rbd pools are always shared"),
            PoolBackend::LvmShared { .. } => Some("lvm-shared pools are always shared"),
            PoolBackend::LvmThin { .. } => Some("lvm-thin pools are always node-local"),
            PoolBackend::Zfs { .. } => Some("zfs pools are always node-local"),
        };
        if let Some(why) = fixed.filter(|_| req.shared.is_some()) {
            return Err(ApiError::bad_request(format!(
                "only dir pools can change shared; {why}"
            )));
        }
        let mut spec = pool.spec.clone();
        spec.shared = req.shared.unwrap_or(spec.shared);
        spec.backups = req.backups.unwrap_or(spec.backups);
        spec.validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if spec == pool.spec {
            return Ok(Json(pool));
        }
        // 공유가 아닌 풀의 백업은 쓴 노드에만 있다(`status.node`). 공유 여부를 바꾸면 그 기록이 틀린다.
        if (pool.spec.backups && !spec.backups) || pool.spec.shared != spec.shared {
            check_no_backups(&state, &pool)?;
        }
        let to_local = pool.spec.shared && !spec.shared;
        if to_local {
            check_pool_local(&state, &pool)?;
        }
        match state
            .cluster
            .update_spec(pool.id, Some(pool.revision), pool.labels.clone(), &spec)
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
        let written = lookup::<StoragePoolSpec>(&state.cluster, &pool.id.to_string())?;
        // `vm_lock`은 이 노드의 요청만 막는다. 검사와 쓰기 사이에 다른 노드가 VM을 옮기거나
        // 마이그레이션을 시작했을 수 있으니 다시 보고, 이제 안 되면 공유로 되돌린다(그 사이 누가 풀을
        // 바꿨으면 그 값을 둔다).
        if to_local && let Err(e) = check_pool_local(&state, &written) {
            let mut spec = written.spec.clone();
            spec.shared = true;
            if let Err(revert) = state
                .cluster
                .update_spec(
                    written.id,
                    Some(written.revision),
                    written.labels.clone(),
                    &spec,
                )
                .await
            {
                tracing::warn!(pool = %written.name, error = %revert, "cannot make the pool shared again");
            }
            return Err(e);
        }
        return Ok(Json(written));
    }
    Err(ApiError::conflict(
        "busy",
        "the pool kept changing while being updated; try again",
    ))
}

/// 공유 풀을 로컬로 바꿔도 되는지. 볼륨은 만든 노드에 있는 것으로 돌아가므로, 그 볼륨을 쓰는 VM이
/// 다른 노드에 있거나 옮겨 가는 중이면 409 `pool_in_use`다.
fn check_pool_local(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    let volumes: Vec<_> = volumes
        .iter()
        .filter(|v| v.spec.pool == pool.name)
        .collect();
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    for vm in &vms {
        for disk in &vm.spec.disks {
            let Some(volume) = volumes.iter().find(|v| v.name == disk.volume) else {
                continue;
            };
            if let (Some(node), Some(home)) = (vm.spec.node, volume.status.node)
                && node != home
            {
                return Err(ApiError::conflict(
                    "pool_in_use",
                    format!(
                        "VM {:?} is on a different node from its volume {:?}; move it back before making pool {:?} local",
                        vm.name, volume.name, pool.name
                    ),
                ));
            }
            let migrating = tasks.iter().any(|t| {
                t.spec.target == vm.id
                    && !t.status.state.is_finished()
                    && matches!(t.spec.op, TaskOp::VmMigrate { .. })
            });
            if migrating {
                return Err(ApiError::conflict(
                    "pool_in_use",
                    format!(
                        "VM {:?} with a disk on pool {:?} is migrating; try again when the migration has finished",
                        vm.name, pool.name
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// 노드에 미리 만든 Linux 브리지를 가리키거나 orbivisord가 만드는 VXLAN 네트워크. 이름 `user`는 예약되어 있다.
/// VNI가 다른 네트워크와 겹치면 409 `vni_in_use`(동시에 만드는 두 요청의 경쟁은 막지 않는다).
#[utoipa::path(post, path = "/networks", tag = "networks",
    request_body = CreateRequest<NetworkSpec>,
    responses((status = 201, body = Resource<NetworkSpec, NetworkStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_network(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<NetworkSpec>>,
) -> Result<(StatusCode, Json<Network>), ApiError> {
    state.authorize(&principal, Action::Write, NetworkSpec::KIND)?;
    check_name(&req.name)?;
    if req.name == USER_NETWORK {
        return Err(ApiError::bad_request(format!(
            "network name {USER_NETWORK:?} is reserved for user-mode NAT"
        )));
    }
    req.spec
        .validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if let Some(vni) = req.spec.vni() {
        let (_, networks) = state.cluster.list::<NetworkSpec>()?;
        if let Some(other) = networks.iter().find(|n| n.spec.vni() == Some(vni)) {
            return Err(ApiError::conflict(
                "vni_in_use",
                format!("VNI {vni} is used by network {:?}", other.name),
            ));
        }
    }
    let mut network = Resource::new(req.name, req.spec);
    network.labels = req.labels;
    network.revision = state.cluster.create(&network).await?;
    Ok((StatusCode::CREATED, Json(network)))
}

/// VM이 쓰고 있으면 409
#[utoipa::path(delete, path = "/networks/{key}", tag = "networks",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_network(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NetworkSpec::KIND)?;
    let network = lookup::<NetworkSpec>(&state.cluster, &key)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.nics.iter().any(|n| n.network == network.name))
    {
        return Err(ApiError::conflict(
            "network_in_use",
            format!("network {:?} is used by VM {:?}", network.name, vm.name),
        ));
    }
    state
        .cluster
        .delete::<NetworkSpec>(network.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 노드의 주소, 유지보수 모드, 역할(`roles`)을 바꾼다. 노드 리소스는 orbivisord가 시작할 때 스스로 만든다.
/// 위트니스 여부(`witness`)는 합류할 때 정해지므로 요청의 값을 무시하고 그대로 둔다. 위트니스는 API가
/// 없으므로 `api` 역할을 줄 수 없다. `roles`를 적으려면 모든 노드가 그 필드를 알아야 한다(`node-roles`).
#[utoipa::path(put, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<NodeSpec>,
    responses((status = 200, body = Resource<NodeSpec, NodeStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<UpdateRequest<NodeSpec>>,
) -> Result<Json<Node>, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let mut spec = NodeSpec {
        witness: node.spec.witness,
        ..req.spec
    };
    if let Some(roles) = &mut spec.roles {
        roles.sort();
        roles.dedup();
        if spec.witness && roles.contains(&NodeRole::Api) {
            return Err(ApiError::bad_request(
                "a witness has no API; it cannot have the api role",
            ));
        }
        // 옛 노드는 `roles`가 든 Node를 풀지 못한다.
        require_feature(&state.cluster, "node-roles")?;
    }
    state
        .cluster
        .update_spec(node.id, if_match(&headers)?, req.labels, &spec)
        .await?;
    Ok(Json(lookup::<NodeSpec>(&state.cluster, &key)?))
}
