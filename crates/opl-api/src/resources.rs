//! 종류에 상관없는 조회 핸들러와 공통 요청 형식, 풀·네트워크·노드 핸들러.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::IF_MATCH;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::{Cluster, Typed};
use opl_types::network::{Network, NetworkSpec, NetworkStatus, USER_NETWORK};
use opl_types::node::{Node, NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec, validate_name};
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, StoragePoolStatus, VolumeSpec, VolumeStatus,
};
use opl_types::task::{TaskSpec, TaskStatus};
use opl_types::vm::{VmSpec, VmStatus};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
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
    // NFS 풀은 늘 공유다. `shared`가 없으면 true로 두고 false면 거절한다.
    if matches!(spec.backend, PoolBackend::Nfs { .. }) {
        if shared_given && !spec.shared {
            return Err(ApiError::bad_request("nfs pools are always shared"));
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

/// 볼륨이 남아 있으면 409
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
    state
        .cluster
        .delete::<StoragePoolSpec>(pool.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 노드에 미리 만든 Linux 브리지를 가리키는 네트워크. 이름 `user`는 예약되어 있다.
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

/// 노드의 주소와 유지보수 모드를 바꾼다. 노드 리소스는 opld가 시작할 때 스스로 만든다.
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
    state
        .cluster
        .update_spec(node.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<NodeSpec>(&state.cluster, &key)?))
}
