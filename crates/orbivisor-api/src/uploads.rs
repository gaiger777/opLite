//! 조각 업로드(계획 77-8): 큰 이미지를 조각으로 올리고 끊기면 받은 데까지에서 잇는다.
//!
//! `POST /uploads`가 확인(옛 `/volumes/import/check`와 같은 권한·이름·풀·형식, 받을 자리의 공간)하고 업로드를 연다.
//! `PUT /uploads/{id}?offset=`가 조각을 받은 데(`offset`)에 이어 쓰고(다르면 409 `upload_offset_mismatch` — 지금
//! 받은 데를 `details.offset`에), `GET /uploads/{id}`가 받은 데를 알린다(끊긴 뒤 이어 올리기). 다 받으면
//! `POST /uploads/{id}/complete`가 볼륨을 만들고 가져오기 Task(202)를 돌려준다. `DELETE`는 버린다.
//!
//! 받는 자리: raw로 두는 파일 풀(디렉터리·NFS)에 raw(ISO 포함)를 올리면 그 풀 안의 임시 이름에 바로 쓰고, 끝내면 볼륨
//! 파일 이름으로 바꾼다(변환·사본 없음 — 가져오기 Task는 확인만 한다). 그 밖(qcow2·vmdk 원본, qcow2 볼륨, 블록·RBD
//! 풀)은 업로드 디렉터리에 받고 가져오기 Task가 변환한다(진행은 Task 진행 `converting`). 업로드는 이 노드의 것이다:
//! 기록은 업로드 디렉터리 `sessions/`의 JSON(노드를 다시 켜도 잇는다)이고, [`UPLOAD_IDLE`] 동안 조각이 오지 않으면
//! 버린다([`sweep`]). 연 사람만 보고 쓴다(남의 업로드는 404).

use std::collections::HashSet;
use std::io::SeekFrom;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use orbivisor_driver::{DirStorage, check_pool_root};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::storage::{SourceFormat, StoragePool, Volume, VolumeContent, VolumeSpec};
use orbivisor_types::task::{Task, TaskOp, now_ms};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Principal;
use crate::error::{ApiError, Json, Query};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::volumes::{ImportQuery, UploadGuard, check_import_target, check_space, content_length};

/// 웹이 보낼 조각 크기(권하는 값)
pub const CHUNK_SIZE: u64 = 16 << 20;
/// 한 번에 받는 조각의 한도
pub const MAX_CHUNK: u64 = 64 << 20;
/// 이만큼 조각이 오지 않으면 업로드를 버린다
pub const UPLOAD_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
/// 버려진 업로드를 찾는 간격
const SWEEP_EVERY: Duration = Duration::from_secs(10 * 60);

/// 이 노드의 업로드: 조각을 쓰고 있는 업로드(한 번에 하나)와 정리 작업
#[derive(Default)]
pub struct Uploads {
    busy: Mutex<HashSet<Uuid>>,
    sweeper: OnceLock<()>,
}

/// `POST /uploads`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    /// 만들 볼륨 이름
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식 (생략하면 풀 종류로: lvm-thin·zfs는 raw, 그 밖은 qcow2. ISO는 늘 raw)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub format: Option<ImageFormat>,
    pub source_format: SourceFormat,
    /// 내용 종류(기본 disk). iso는 dir·nfs 풀에 raw로 둔다.
    #[serde(default)]
    pub content: VolumeContent,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// 진행 중인 업로드
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Upload {
    pub id: Uuid,
    /// 만들 볼륨 이름
    pub name: String,
    pub pool: String,
    /// 만들 볼륨의 형식(정해진 것)
    pub format: ImageFormat,
    pub source_format: SourceFormat,
    pub content: VolumeContent,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
    /// 받은 바이트 — 다음 조각은 여기서부터
    pub offset: u64,
    /// 권하는 조각 크기(바이트). 한 조각은 [`MAX_CHUNK`]까지 받는다.
    pub chunk_size: u64,
    /// 대상 풀에 바로 쓰는지(raw·ISO를 파일 풀에 — 끝낸 뒤 변환하지 않는다)
    pub direct: bool,
    /// 연 시각·마지막 조각 시각(유닉스 ms)
    pub created_at: u64,
    pub updated_at: u64,
    /// 조각이 더 오지 않으면 버리는 시각(유닉스 ms)
    pub expires_at: u64,
}

/// 디스크의 기록(응답 + 주인·받는 파일)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    upload: Upload,
    owner: Uuid,
    data: PathBuf,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ChunkQuery {
    /// 이 조각이 시작하는 바이트(지금 받은 데와 같아야 한다)
    pub offset: u64,
}

/// 업로드 목록(`GET /uploads`)
#[derive(Debug, Serialize, ToSchema)]
pub struct UploadList {
    pub items: Vec<Upload>,
}

fn sessions_dir(upload_dir: &FsPath) -> PathBuf {
    upload_dir.join("sessions")
}

fn record_path(upload_dir: &FsPath, id: Uuid) -> PathBuf {
    sessions_dir(upload_dir).join(format!("{id}.json"))
}

/// 바로 쓰는 업로드가 받는 풀 안 파일(끝내면 볼륨 파일 이름으로 바꾼다)
fn direct_part(root: &FsPath, id: Uuid) -> PathBuf {
    root.join(format!(".upload-{id}.raw.part"))
}

/// raw로 두는 파일 풀에 raw 원본이면 그 풀 루트(이 노드에서 쓸 수 있을 때)
fn direct_root(
    state: &AppState,
    pool: &StoragePool,
    format: ImageFormat,
    source: SourceFormat,
) -> Option<PathBuf> {
    (format == ImageFormat::Raw && source == SourceFormat::Raw)
        .then(|| check_pool_root(pool, &state.mount_root).ok())
        .flatten()
}

fn not_found(id: Uuid) -> ApiError {
    ApiError::not_found(format!("upload {id} not found"))
}

async fn read_record(upload_dir: &FsPath, id: Uuid) -> Option<Record> {
    let text = tokio::fs::read(record_path(upload_dir, id)).await.ok()?;
    serde_json::from_slice(&text).ok()
}

async fn write_record(upload_dir: &FsPath, record: &Record) -> Result<(), ApiError> {
    let path = record_path(upload_dir, record.upload.id);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_vec(record).map_err(ApiError::internal)?;
    tokio::fs::write(&tmp, text)
        .await
        .map_err(ApiError::internal)?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(ApiError::internal)
}

/// 받은 바이트(받는 파일의 길이)
async fn received(data: &FsPath) -> u64 {
    tokio::fs::metadata(data)
        .await
        .map(|m| m.len())
        .unwrap_or(0)
}

/// 부른 사람의 살아 있는 업로드. 없거나 남의 것이면 404, 시간이 지났으면 버리고 404.
async fn own(state: &AppState, principal: &Principal, id: Uuid) -> Result<Record, ApiError> {
    let mut record = read_record(&state.upload_dir, id)
        .await
        .filter(|r| r.owner == principal.user)
        .ok_or_else(|| not_found(id))?;
    if record.upload.expires_at <= now_ms() {
        discard(&state.upload_dir, &record).await;
        return Err(not_found(id));
    }
    record.upload.offset = received(&record.data).await;
    Ok(record)
}

async fn discard(upload_dir: &FsPath, record: &Record) {
    let _ = tokio::fs::remove_file(&record.data).await;
    let _ = tokio::fs::remove_file(record_path(upload_dir, record.upload.id)).await;
}

/// 조각을 쓰거나 끝내는 동안 같은 업로드의 다른 요청을 막는다(409 `upload_busy`).
struct Busy<'a> {
    uploads: &'a Uploads,
    id: Uuid,
}

impl<'a> Busy<'a> {
    fn take(uploads: &'a Uploads, id: Uuid) -> Result<Self, ApiError> {
        if !uploads.busy.lock().unwrap().insert(id) {
            return Err(ApiError::conflict(
                "upload_busy",
                format!("upload {id} is receiving another request"),
            ));
        }
        Ok(Self { uploads, id })
    }
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.uploads.busy.lock().unwrap().remove(&self.id);
    }
}

/// 처음 쓸 때 버려진 업로드를 찾는 작업을 띄운다(상태가 사라지면 끝난다).
fn start_sweeper(state: &Arc<AppState>) {
    if state.uploads.sweeper.set(()).is_err() {
        return;
    }
    let weak: Weak<AppState> = Arc::downgrade(state);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            let Some(state) = weak.upgrade() else { return };
            let dir = state.upload_dir.clone();
            drop(state);
            let _ = tokio::task::spawn_blocking(move || sweep(&dir, now_ms())).await;
        }
    });
}

/// 시간이 지났거나 받는 파일이 사라진(끝내다 멈춘) 업로드를 지운다. 노드가 켜질 때(Task 실행기의 고아 정리)와
/// [`SWEEP_EVERY`]마다 돈다.
pub(crate) fn sweep(upload_dir: &FsPath, now: u64) {
    let Ok(entries) = std::fs::read_dir(sessions_dir(upload_dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json.tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if !name.ends_with(".json") {
            // 기록 없는 받는 파일
            let id = name
                .strip_suffix(".part")
                .and_then(|n| Uuid::try_parse(n).ok());
            if id.is_none_or(|id| !record_path(upload_dir, id).exists()) {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        let record = std::fs::read(&path)
            .ok()
            .and_then(|t| serde_json::from_slice::<Record>(&t).ok());
        let stale = match &record {
            None => true,
            Some(r) => r.upload.expires_at <= now || !r.data.exists(),
        };
        if stale {
            if let Some(r) = &record {
                tracing::info!(upload = %r.upload.id, name = %r.upload.name, "removing an abandoned upload");
                let _ = std::fs::remove_file(&r.data);
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// 업로드를 연다(201). 확인은 옛 업로드(`/volumes/import/check`)와 같고, 공간은 받을 자리(바로 쓰면 풀, 아니면 업로드
/// 디렉터리)에서 본다. 같은 이름의 업로드가 이미 열려 있으면 409 `name_taken`.
#[utoipa::path(post, path = "/uploads", tag = "storage",
    request_body = UploadRequest,
    responses((status = 201, body = Upload), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<UploadRequest>,
) -> Result<(StatusCode, Json<Upload>), ApiError> {
    start_sweeper(&state);
    let q = ImportQuery {
        name: req.name,
        pool: req.pool,
        format: req.format,
        source_format: req.source_format,
        content: req.content,
    };
    let (pool, format) = check_import_target(&state, &principal, &q)?;
    let sessions = sessions_dir(&state.upload_dir);
    tokio::fs::create_dir_all(&sessions)
        .await
        .map_err(ApiError::internal)?;
    if live_records(&state.upload_dir)
        .await
        .iter()
        .any(|r| r.upload.name == q.name)
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("an upload of volume {:?} is in progress", q.name),
        ));
    }
    let id = Uuid::now_v7();
    let root = direct_root(&state, &pool, format, q.source_format);
    let data = match &root {
        Some(root) => direct_part(root, id),
        None => sessions.join(format!("{id}.part")),
    };
    check_space(data.parent().expect("a file in a directory"), req.size).await?;
    let now = now_ms();
    let record = Record {
        upload: Upload {
            id,
            name: q.name,
            pool: pool.name,
            format,
            source_format: q.source_format,
            content: q.content,
            size: req.size,
            offset: 0,
            chunk_size: CHUNK_SIZE,
            direct: root.is_some(),
            created_at: now,
            updated_at: now,
            expires_at: now + UPLOAD_IDLE.as_millis() as u64,
        },
        owner: principal.user,
        data,
    };
    tokio::fs::File::create_new(&record.data)
        .await
        .map_err(ApiError::internal)?;
    if let Err(e) = write_record(&state.upload_dir, &record).await {
        let _ = tokio::fs::remove_file(&record.data).await;
        return Err(e);
    }
    Ok((StatusCode::CREATED, Json(record.upload)))
}

/// 이 노드의 살아 있는 업로드 기록(모두)
async fn live_records(upload_dir: &FsPath) -> Vec<Record> {
    let Ok(mut entries) = tokio::fs::read_dir(sessions_dir(upload_dir)).await else {
        return Vec::new();
    };
    let now = now_ms();
    let mut out = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !entry.file_name().to_string_lossy().ends_with(".json") {
            continue;
        }
        if let Some(record) = tokio::fs::read(entry.path())
            .await
            .ok()
            .and_then(|t| serde_json::from_slice::<Record>(&t).ok())
            && record.upload.expires_at > now
        {
            out.push(record);
        }
    }
    out
}

/// 부른 사람이 이 노드에 연 업로드(연 차례). 탭을 닫았다 와도 같은 파일(이름·크기)을 고르면 잇는 데 쓴다.
#[utoipa::path(get, path = "/uploads", tag = "storage",
    responses((status = 200, body = UploadList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_uploads(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<UploadList>, ApiError> {
    let mut items = Vec::new();
    for mut record in live_records(&state.upload_dir).await {
        if record.owner == principal.user {
            record.upload.offset = received(&record.data).await;
            items.push(record.upload);
        }
    }
    items.sort_by_key(|u| (u.created_at, u.id));
    Ok(Json(UploadList { items }))
}

/// 업로드와 받은 바이트(`offset` — 끊긴 뒤 여기서 잇는다)
#[utoipa::path(get, path = "/uploads/{id}", tag = "storage",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Upload), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<Json<Upload>, ApiError> {
    Ok(Json(own(&state, &principal, id).await?.upload))
}

/// 조각 하나를 받은 데(`offset`)에 이어 쓴다(200, 새 `offset`). `offset`이 받은 데와 다르면(순서가 어긋남·같은 조각을
/// 다시 보냄) 409 `upload_offset_mismatch`(`details.offset` = 받은 데). 크기를 넘으면 400, 조각이
/// [`MAX_CHUNK`]보다 크면 413, 공간이 모자라면 507. 중간에 끊기면 그때까지 받은 것은 남는다(`GET`의 `offset`).
#[utoipa::path(put, path = "/uploads/{id}", tag = "storage",
    params(("id" = Uuid, Path), ChunkQuery),
    request_body(content = crate::volumes::ImageFile, content_type = "application/octet-stream"),
    responses((status = 200, body = Upload), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_upload_chunk(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<Uuid>,
    Query(q): Query<ChunkQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Upload>, ApiError> {
    let length = content_length(&headers)?;
    let _busy = Busy::take(&state.uploads, id)?;
    let mut record = own(&state, &principal, id).await?;
    let offset = record.upload.offset;
    if q.offset != offset {
        let mut e = ApiError::conflict(
            "upload_offset_mismatch",
            format!("upload {id} has {offset} bytes; send the chunk that starts there"),
        );
        e.details = Some(serde_json::json!({ "offset": offset }));
        return Err(e);
    }
    if length > MAX_CHUNK {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "chunk_too_large",
            format!("a chunk can be at most {MAX_CHUNK} bytes"),
        ));
    }
    if offset.saturating_add(length) > record.upload.size {
        return Err(ApiError::bad_request(format!(
            "the chunk ends past the upload size {}",
            record.upload.size
        )));
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(&record.data)
        .await
        .map_err(|_| not_found(id))?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(ApiError::internal)?;
    let mut got = 0u64;
    let mut stream = body.into_data_stream();
    let result = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::bad_request(format!("upload failed: {e}")))?;
            got += chunk.len() as u64;
            if got > length {
                return Err(ApiError::bad_request(
                    "the chunk is longer than Content-Length",
                ));
            }
            file.write_all(&chunk).await.map_err(write_error)?;
        }
        Ok(())
    }
    .await;
    // 받은 것은 남긴다(이어 올리기). 디스크에 닿은 뒤에 받은 데로 알린다.
    file.sync_data().await.map_err(write_error)?;
    if got > 0 {
        record.upload.updated_at = now_ms();
        record.upload.expires_at = record.upload.updated_at + UPLOAD_IDLE.as_millis() as u64;
        write_record(&state.upload_dir, &record).await?;
    }
    result?;
    if got != length {
        return Err(ApiError::bad_request("the chunk ended early"));
    }
    record.upload.offset = offset + got;
    Ok(Json(record.upload))
}

fn write_error(e: std::io::Error) -> ApiError {
    if e.kind() == std::io::ErrorKind::StorageFull {
        ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            "not enough free space for the upload",
        )
    } else {
        ApiError::internal(e)
    }
}

/// 다 받은 업로드로 볼륨을 만들고 가져오기 Task를 돌려준다(202). 바로 쓴 업로드는 받은 파일을 볼륨 파일 이름으로
/// 바꾸고(Task는 확인만), 아니면 Task가 변환한다. 덜 받았으면 409 `upload_incomplete`. 권한·이름·풀은 다시 본다.
#[utoipa::path(post, path = "/uploads/{id}/complete", tag = "storage",
    params(("id" = Uuid, Path)),
    responses((status = 202, body = Resource<orbivisor_types::task::TaskSpec, orbivisor_types::task::TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn complete_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let _busy = Busy::take(&state.uploads, id)?;
    let record = own(&state, &principal, id).await?;
    let u = &record.upload;
    if u.offset != u.size {
        let mut e = ApiError::conflict(
            "upload_incomplete",
            format!("upload {id} has {} of {} bytes", u.offset, u.size),
        );
        e.details = Some(serde_json::json!({ "offset": u.offset }));
        return Err(e);
    }
    let q = ImportQuery {
        name: u.name.clone(),
        pool: u.pool.clone(),
        format: Some(u.format),
        source_format: u.source_format,
        content: u.content,
    };
    let (pool, format) = check_import_target(&state, &principal, &q)?;
    let mut volume: Volume = Resource::new(
        q.name,
        VolumeSpec {
            pool: pool.name.clone(),
            size_bytes: 0,
            format,
            content: q.content,
            base: None,
        },
    );
    volume.status.node = Some(state.node);
    let source = if u.direct {
        DirStorage::volume_path(&pool, &state.mount_root, volume.id, format)
    } else {
        state.upload_dir.join(format!("{}.upload", volume.id))
    };
    state.cluster.create(&volume).await?;
    // 여기서부터 실패하면 볼륨과 옮긴 파일을 지운다.
    let mut guard = UploadGuard {
        state: state.clone(),
        volume: volume.id,
        source: source.clone(),
        armed: true,
    };
    tokio::fs::rename(&record.data, &source)
        .await
        .map_err(ApiError::internal)?;
    let op = TaskOp::VolumeImport {
        source,
        source_format: q.source_format,
    };
    let task = create_task(&state.cluster, op, volume.id, state.node).await?;
    guard.armed = false;
    let _ = tokio::fs::remove_file(record_path(&state.upload_dir, id)).await;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 업로드를 버린다(204, 받은 것도 지운다).
#[utoipa::path(delete, path = "/uploads/{id}", tag = "storage",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let _busy = Busy::take(&state.uploads, id)?;
    let record = own(&state, &principal, id).await?;
    discard(&state.upload_dir, &record).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(dir: &FsPath, id: Uuid, expires_at: u64, data: PathBuf) -> Record {
        Record {
            upload: Upload {
                id,
                name: "disk".into(),
                pool: "local".into(),
                format: ImageFormat::Raw,
                source_format: SourceFormat::Raw,
                content: VolumeContent::Disk,
                size: 10,
                offset: 0,
                chunk_size: CHUNK_SIZE,
                direct: false,
                created_at: 0,
                updated_at: 0,
                expires_at,
            },
            owner: Uuid::nil(),
            data: dir.join(data),
        }
    }

    /// 시간이 지난 업로드, 받는 파일이 사라진 기록, 기록 없는 받는 파일·쓰다 만 기록을 지우고 살아 있는 것은 둔다.
    #[test]
    fn sweep_removes_abandoned_uploads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sessions = sessions_dir(dir);
        std::fs::create_dir_all(&sessions).unwrap();
        let (live, old, lost, orphan) = (
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
            Uuid::now_v7(),
        );
        for (id, expires, data) in [
            (live, 2_000, true),
            (old, 1_000, true),
            (lost, 2_000, false),
        ] {
            let r = record(&sessions, id, expires, format!("{id}.part").into());
            if data {
                std::fs::write(&r.data, b"x").unwrap();
            }
            std::fs::write(record_path(dir, id), serde_json::to_vec(&r).unwrap()).unwrap();
        }
        std::fs::write(sessions.join(format!("{orphan}.part")), b"x").unwrap();
        std::fs::write(sessions.join("x.json.tmp"), b"{").unwrap();
        std::fs::write(sessions.join("junk.json"), b"not json").unwrap();

        sweep(dir, 1_500);
        let mut left: Vec<String> = std::fs::read_dir(&sessions)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [format!("{live}.json"), format!("{live}.part")]);
        // 업로드 디렉터리가 없어도 된다.
        sweep(&dir.join("missing"), 0);
    }

    #[test]
    fn records_keep_the_upload_flat() {
        let tmp = tempfile::tempdir().unwrap();
        let r = record(tmp.path(), Uuid::nil(), 5, "d".into());
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["name"], "disk");
        assert_eq!(json["owner"], Uuid::nil().to_string());
        let back: Record = serde_json::from_value(json).unwrap();
        assert_eq!(back.upload, r.upload);
    }
}
