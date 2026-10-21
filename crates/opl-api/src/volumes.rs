//! 볼륨 생성·가져오기·삭제. 파일 작업은 풀을 쓸 수 있는 노드의 Task 실행기가 한다.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::runtime::ImageFormat;
use opl_types::storage::{SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, check_name, lookup};
use crate::state::AppState;
use crate::tasks::create_task;

/// 볼륨 파일 작업을 실행할 노드. 이 노드가 풀을 쓸 수 있으면 이 노드다.
fn pool_node(state: &AppState, pool: &StoragePool) -> Uuid {
    let nodes = &pool.spec.nodes;
    if nodes.is_empty() || nodes.contains(&state.node) {
        state.node
    } else {
        nodes[0]
    }
}

fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
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
    check_name(&req.name)?;
    if req.spec.size_bytes == 0 {
        return Err(ApiError::bad_request("size_bytes must be greater than 0"));
    }
    let pool = find_pool(&state, &req.spec.pool)?;
    let mut volume: Volume = Resource::new(req.name, req.spec);
    volume.labels = req.labels;
    state.cluster.create(&volume).await?;
    let task = create_task(
        &state.cluster,
        TaskOp::VolumeCreate,
        volume.id,
        pool_node(&state, &pool),
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ImportQuery {
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (기본 qcow2)
    #[serde(default = "qcow2")]
    #[param(inline)]
    pub format: ImageFormat,
    #[param(inline)]
    pub source_format: SourceFormat,
}

fn qcow2() -> ImageFormat {
    ImageFormat::Qcow2
}

/// 요청 본문(이미지 파일)을 이 노드에 받아 두고, 그것을 변환해 볼륨을 채울 Task를 돌려준다(202).
/// 풀은 이 노드에서 쓸 수 있어야 한다.
#[utoipa::path(post, path = "/volumes/import", tag = "storage",
    params(ImportQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn import_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ImportQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    check_name(&q.name)?;
    let pool = find_pool(&state, &q.pool)?;
    if pool_node(&state, &pool) != state.node {
        return Err(ApiError::conflict(
            "pool_not_on_node",
            format!(
                "storage pool {:?} is not available on this node; upload to one of its nodes",
                pool.name
            ),
        ));
    }
    // 크기는 가져온 뒤 실제 이미지 크기로 바뀐다.
    let volume: Volume = Resource::new(
        q.name,
        VolumeSpec {
            pool: pool.name.clone(),
            size_bytes: 0,
            format: q.format,
        },
    );
    let length = content_length(&headers)?;
    tokio::fs::create_dir_all(&state.upload_dir)
        .await
        .map_err(ApiError::internal)?;
    let available = fs4::available_space(&state.upload_dir).map_err(ApiError::internal)?;
    if length.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!("not enough free space for a {length}-byte upload"),
        ));
    }
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

/// VM이 쓰지 않는 볼륨을 지울 Task를 돌려준다(202).
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
    let node = match state
        .cluster
        .get_by_name::<StoragePoolSpec>(&volume.spec.pool)?
    {
        Some(pool) => pool_node(&state, &pool),
        None => state.node,
    };
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}
