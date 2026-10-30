//! 볼륨 생성·가져오기·삭제. 파일 작업은 풀을 쓸 수 있는 노드의 Task 실행기가 한다.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::snapshot::SnapshotSpec;
use opl_types::storage::{
    PoolBackend, SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumePhase, VolumeSpec,
    is_shared, pool_error_on, pool_listed_on, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, check_name, lookup};

/// `/volumes/import` 경로와 겹치므로 볼륨 이름으로 쓸 수 없다.
const RESERVED_NAME: &str = "import";

fn check_volume_name(name: &str) -> Result<(), ApiError> {
    check_name(name)?;
    if name == RESERVED_NAME {
        return Err(ApiError::bad_request(format!(
            "the name {RESERVED_NAME} is reserved"
        )));
    }
    Ok(())
}
use crate::snapshots::vm_placement;
use crate::state::AppState;
use crate::tasks::create_task;

/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. 그런 노드가 없으면 409 `pool_unavailable`.
fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    usable_node(&state.cluster, pool, Some(state.node))?.ok_or_else(|| {
        ApiError::conflict(
            "pool_unavailable",
            format!("storage pool {:?} is not ready on any node", pool.name),
        )
    })
}

/// 풀을 쓸 수 있는 온라인 노드: `prefer`가 그런 노드면 그 노드, 아니면 id가 가장 작은 노드.
pub(crate) fn usable_node(
    cluster: &Cluster,
    pool: &StoragePool,
    prefer: Option<Uuid>,
) -> Result<Option<Uuid>, ClusterError> {
    let (_, nodes) = cluster.list::<NodeSpec>()?;
    let usable: Vec<Uuid> = nodes
        .iter()
        .filter(|n| n.status.online && pool_usable_on(pool, n))
        .map(|n| n.id)
        .collect();
    if let Some(node) = prefer.filter(|n| usable.contains(n)) {
        return Ok(Some(node));
    }
    Ok(usable.into_iter().min())
}

/// 볼륨 파일 작업을 실행할 노드. 공유가 아닌 풀은 파일이 있는 노드(볼륨을 만든 노드)이고, 공유
/// 풀은 [`pool_node`]의 규칙이다. 풀이 없어졌거나 파일이 있던 노드가 클러스터에서 빠졌으면 이
/// 노드다(파일이 없으니 리소스만 지운다).
fn volume_node(state: &AppState, volume: &Volume) -> Result<Uuid, ApiError> {
    match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
    {
        Some(pool) => match volume.status.node {
            Some(node) if !is_shared(&pool) => {
                if state.cluster.get::<NodeSpec>(node)?.is_some() {
                    Ok(node)
                } else {
                    Ok(state.node)
                }
            }
            _ => pool_node(state, &pool),
        },
        None => Ok(state.node),
    }
}

/// `node`에서 풀을 쓸 수 있는지 확인한다. 노드 목록에 없으면 409 `pool_not_on_node`(메시지는
/// `not_on_node`), 있지만 그 노드에서 준비되지 않았으면 409 `pool_unavailable`(노드가 알린 오류).
pub(crate) fn check_pool_on(
    state: &AppState,
    pool: &StoragePool,
    node: Uuid,
    not_on_node: impl FnOnce() -> String,
) -> Result<(), ApiError> {
    if !pool_listed_on(pool, node) {
        return Err(ApiError::conflict("pool_not_on_node", not_on_node()));
    }
    let Some(n) = state.cluster.get::<NodeSpec>(node)? else {
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} is not ready: node {node} not found",
                pool.name
            ),
        ));
    };
    if !pool_usable_on(pool, &n) {
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!(
                "storage pool {:?} is not ready on node {:?}: {}",
                pool.name,
                n.name,
                pool_error_on(pool, &n)
            ),
        ));
    }
    Ok(())
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS, RBD)은 raw만 담는다.
fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    let kind = match pool.spec.backend {
        PoolBackend::LvmThin { .. } => "lvm-thin",
        PoolBackend::Zfs { .. } => "zfs",
        PoolBackend::Rbd { .. } => "rbd",
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => return Ok(()),
    };
    if format != ImageFormat::Raw {
        return Err(ApiError::bad_request(format!(
            "{kind} pools hold raw volumes"
        )));
    }
    Ok(())
}

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀(LVM-thin, ZFS, RBD)은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
fn default_format(pool: &StoragePool) -> ImageFormat {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } | PoolBackend::Zfs { .. } | PoolBackend::Rbd { .. } => {
            ImageFormat::Raw
        }
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => ImageFormat::Qcow2,
    }
}

/// 볼륨 리소스를 만들고, 파일을 만들 Task를 돌려준다(202). Task가 실패하면 리소스도 지운다.
#[utoipa::path(post, path = "/volumes", tag = "storage",
    request_body = CreateRequest<VolumeSpec>,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<CreateRequest<VolumeSpec>>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_volume_name(&req.name)?;
    if req.spec.size_bytes == 0 {
        return Err(ApiError::bad_request("size_bytes must be greater than 0"));
    }
    let pool = find_pool(&state, &req.spec.pool)?;
    check_pool_format(&pool, req.spec.format)?;
    let node = pool_node(&state, &pool)?;
    let mut volume: Volume = Resource::new(req.name, req.spec);
    volume.labels = req.labels;
    volume.status.node = Some(node);
    state.cluster.create(&volume).await?;
    let task = create_task(&state.cluster, TaskOp::VolumeCreate, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ImportQuery {
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2)
    #[serde(default)]
    #[param(inline)]
    pub format: Option<ImageFormat>,
    #[param(inline)]
    pub source_format: SourceFormat,
}

/// 업로드 사전 확인의 매개변수. 업로드의 것에 올릴 파일 크기(바이트)를 더한다.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ImportCheckQuery {
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2)
    #[serde(default)]
    #[param(inline)]
    pub format: Option<ImageFormat>,
    #[param(inline)]
    pub source_format: SourceFormat,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// 업로드를 받기 전의 확인. 사전 확인과 업로드가 함께 쓴다. 문제가 없으면 풀과 만들 볼륨의
/// 형식을 돌려준다. 확인 뒤에 다른 요청이 이름을 가져가면 저장소에 만들 때 `name_taken`이 난다.
pub async fn check_import(
    state: &AppState,
    principal: &Principal,
    q: &ImportQuery,
    size: u64,
) -> Result<(StoragePool, ImageFormat), ApiError> {
    state.authorize(principal, Action::Write, VolumeSpec::KIND)?;
    check_volume_name(&q.name)?;
    let pool = find_pool(state, &q.pool)?;
    let format = q.format.unwrap_or_else(|| default_format(&pool));
    check_pool_format(&pool, format)?;
    check_pool_on(state, &pool, state.node, || {
        format!(
            "storage pool {:?} is not available on this node; upload to one of its nodes",
            pool.name
        )
    })?;
    if state.cluster.get_by_name::<VolumeSpec>(&q.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("volume {:?} already exists", q.name),
        ));
    }
    tokio::fs::create_dir_all(&state.upload_dir)
        .await
        .map_err(ApiError::internal)?;
    let available = fs4::available_space(&state.upload_dir).map_err(ApiError::internal)?;
    if size.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("not enough free space for a {size}-byte upload"),
        ));
    }
    Ok((pool, format))
}

/// 업로드 전에 받을 수 있는지 확인한다(204). 거절 이유는 업로드와 같다. 본문은 받지 않는다.
#[utoipa::path(post, path = "/volumes/import/check", tag = "storage",
    params(ImportCheckQuery),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_import_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ImportCheckQuery>,
) -> Result<StatusCode, ApiError> {
    let size = q.size;
    let q = ImportQuery {
        name: q.name,
        pool: q.pool,
        format: q.format,
        source_format: q.source_format,
    };
    check_import(&state, &principal, &q, size).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 업로드 본문의 문서용 스키마: 바이너리 파일
#[derive(ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct ImageFile(#[allow(dead_code)] Vec<u8>);

/// 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
/// 풀은 이 노드에서 쓸 수 있어야 한다. 받기 전에 `check_import`로 확인한다.
#[utoipa::path(post, path = "/volumes/import", tag = "storage",
    params(ImportQuery),
    request_body(content = ImageFile, content_type = "application/octet-stream"),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn import_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ImportQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let length = content_length(&headers)?;
    let (pool, format) = check_import(&state, &principal, &q, length).await?;
    // 크기는 가져온 뒤 실제 이미지 크기로 바뀐다.
    let mut volume: Volume = Resource::new(
        q.name,
        VolumeSpec {
            pool: pool.name.clone(),
            size_bytes: 0,
            format,
        },
    );
    volume.status.node = Some(state.node);
    state.cluster.create(&volume).await?;
    // 여기서부터 실패하거나 클라이언트가 끊어 이 future가 버려지면 볼륨과 파일을 지운다.
    let mut guard = UploadGuard {
        state: state.clone(),
        volume: volume.id,
        source: state.upload_dir.join(format!("{}.upload", volume.id)),
        armed: true,
    };
    receive(&guard.source, body, length).await?;
    let op = TaskOp::VolumeImport {
        source: guard.source.clone(),
        source_format: q.source_format,
    };
    let task = create_task(&state.cluster, op, volume.id, state.node).await?;
    guard.armed = false;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 올린 파일이 다 들어와도 데이터 디렉터리(Raft 저장소가 있다)에 남겨 둘 여유 공간
const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;

fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::LENGTH_REQUIRED,
                "length_required",
                "uploads need a Content-Length header",
            )
        })
}

/// 업로드를 끝내지 못한 볼륨과 파일을 지운다. 정상 경로에서는 `armed`를 내린다.
struct UploadGuard {
    state: Arc<AppState>,
    volume: Uuid,
    source: PathBuf,
    armed: bool,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (state, volume, source) = (self.state.clone(), self.volume, self.source.clone());
        tokio::spawn(async move {
            let _ = tokio::fs::remove_file(&source).await;
            if let Err(e) = state.cluster.delete::<VolumeSpec>(volume, None).await {
                tracing::warn!(%volume, error = %e, "cannot remove volume of a failed upload");
            }
        });
    }
}

/// 본문을 `length`바이트까지 받는다. 그보다 길거나 짧으면 실패다.
async fn receive(path: &FsPath, body: Body, length: u64) -> Result<(), ApiError> {
    let mut file = tokio::fs::File::create_new(path)
        .await
        .map_err(ApiError::internal)?;
    let mut received = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload failed: {e}")))?;
        received += chunk.len() as u64;
        if received > length {
            return Err(ApiError::bad_request(
                "upload is longer than Content-Length",
            ));
        }
        file.write_all(&chunk).await.map_err(ApiError::internal)?;
    }
    if received != length {
        return Err(ApiError::bad_request("upload ended early"));
    }
    file.sync_all().await.map_err(ApiError::internal)?;
    Ok(())
}

/// VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202). 아직 만들고 있는 볼륨은 지울 수 없다.
/// 스냅샷의 VM 스펙이 가리키면 409 `volume_in_snapshot`.
#[utoipa::path(delete, path = "/volumes/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err(ApiError::conflict(
            "volume_not_ready",
            format!("volume {:?} is still being created", volume.name),
        ));
    }
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(vm) = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(ApiError::conflict(
            "volume_in_use",
            format!("volume {:?} is attached to VM {:?}", volume.name, vm.name),
        ));
    }
    let (_, snapshots) = state.cluster.list::<SnapshotSpec>()?;
    if let Some(s) = snapshots
        .iter()
        .find(|s| s.spec.vm_spec.disks.iter().any(|d| d.volume == volume.name))
    {
        return Err(ApiError::conflict(
            "volume_in_snapshot",
            format!(
                "volume {:?} is used by snapshot {:?} of VM {}",
                volume.name, s.spec.name, s.spec.vm
            ),
        ));
    }
    let node = volume_node(&state, &volume)?;
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizeRequest {
    /// 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
    pub size_bytes: u64,
}

/// 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). VM이 쓰면 그 VM의 스냅샷
/// Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
/// 거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다.
#[utoipa::path(post, path = "/volumes/{key}/resize", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    request_body = ResizeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn resize_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<ResizeRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err(ApiError::conflict(
            "volume_not_ready",
            format!("volume {:?} is still being created", volume.name),
        ));
    }
    if req.size_bytes <= volume.spec.size_bytes {
        return Err(ApiError::bad_request(format!(
            "size_bytes must be larger than the current size ({} bytes)",
            volume.spec.size_bytes
        )));
    }
    // qcow2는 `qemu-img resize`도 QMP `block_resize`도 512 배수가 아니면 거부한다.
    if req.size_bytes % 512 != 0 {
        return Err(ApiError::bad_request(
            "size_bytes must be a multiple of 512",
        ));
    }
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let user = vms
        .iter()
        .find(|vm| vm.spec.disks.iter().any(|d| d.volume == volume.name));
    let node = match user.and_then(|vm| vm_placement(&state.cluster, Some(state.node), vm)) {
        Some(node) => node,
        None => volume_node(&state, &volume)?,
    };
    let op = TaskOp::VolumeResize {
        size_bytes: req.size_bytes,
    };
    let task = create_task(&state.cluster, op, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
