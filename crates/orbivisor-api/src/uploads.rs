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

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use orbivisor_cluster::Cluster;
use orbivisor_driver::{DirStorage, check_pool_root};
use orbivisor_types::authz::Privilege;
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::storage::{
    SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeContent, VolumeSpec,
};
use orbivisor_types::task::{Task, TaskOp, now_ms};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json, Query};
use crate::state::AppState;
use crate::tasks::create_task;
use crate::volumes::{
    ImportQuery, UPLOAD_RESERVE_BYTES, UploadGuard, check_import_target, check_space,
    content_length, find_pool,
};

/// 웹이 보낼 조각 크기(권하는 값)
pub const CHUNK_SIZE: u64 = 16 << 20;
/// 한 번에 받는 조각의 한도
pub const MAX_CHUNK: u64 = 64 << 20;
/// 이만큼 조각이 오지 않으면 업로드를 버린다
pub const UPLOAD_IDLE: Duration = Duration::from_secs(24 * 60 * 60);
/// 버려진 업로드를 찾는 간격
const SWEEP_EVERY: Duration = Duration::from_secs(10 * 60);
/// 풀 경로(NFS일 수 있다)의 파일시스템 호출을 기다리는 시간 — 드라이버의 `on_pool`과 같다(리뷰 77-8 I4).
const FS_TIMEOUT: Duration = Duration::from_secs(30);
/// 기록 없는 받는 파일·쓰다 만 기록은 이만큼 지난 뒤에 지운다(여는 중 — 받는 파일을 만들고 기록을 쓰기 전 — 을 건드리지 않게).
const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);

/// 이 노드의 업로드: 조각을 쓰거나 끝내는 업로드(한 번에 하나), 받은 데까지의 SHA-256, 열기 잠금, 정리 작업
#[derive(Default)]
pub struct Uploads {
    busy: Mutex<HashMap<Uuid, Activity>>,
    /// 받은 데(바이트)와 거기까지의 SHA-256 — 이 프로세스가 받은 것. 노드를 다시 켰거나 쓰다 실패해 어긋나면 끝낼 때
    /// 받은 파일 전체를 다시 읽어 센다.
    hashes: Mutex<HashMap<Uuid, (u64, Sha256)>>,
    /// 여는 확인(같은 이름·공간 예약)과 받는 파일·기록 만들기를 한 번에 하나씩(리뷰 77-8 S5)
    opening: tokio::sync::Mutex<()>,
    sweeper: OnceLock<()>,
}

/// 쓰거나 끝내는 중인 업로드
#[derive(Default)]
struct Activity {
    /// 끝내는 중(볼륨·Task를 만드는 중 — 버리지 못한다)
    completing: bool,
    /// 쓰는 중에 버리기(`DELETE`)가 왔다 — 조각이 끝나면 이 받는 파일을 지운다(리뷰 77-8 I1)
    doomed: Option<PathBuf>,
}

impl Uploads {
    /// `at`바이트까지의 해시(이어 셀 수 있으면). 처음(0)이면 새로 센다.
    fn take_hash(&self, id: Uuid, at: u64) -> Option<Sha256> {
        match self.hashes.lock().unwrap().remove(&id) {
            Some((done, hash)) if done == at => Some(hash),
            _ if at == 0 => Some(Sha256::new()),
            _ => None,
        }
    }

    fn keep_hash(&self, id: Uuid, at: u64, hash: Sha256) {
        self.hashes.lock().unwrap().insert(id, (at, hash));
    }

    /// 다 받은 파일의 SHA-256(16진) — 받으며 셌으면
    fn finished_hash(&self, id: Uuid, size: u64) -> Option<String> {
        let hashes = self.hashes.lock().unwrap();
        let (done, hash) = hashes.get(&id)?;
        (*done == size).then(|| hex::encode(hash.clone().finalize()))
    }

    fn forget(&self, id: Uuid) {
        self.hashes.lock().unwrap().remove(&id);
    }

    fn is_busy(&self, id: Uuid) -> bool {
        self.busy.lock().unwrap().contains_key(&id)
    }
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
    /// 올릴 파일 크기(바이트, 1 이상)
    pub size: u64,
    /// 올릴 파일의 지문 — 서버는 두었다 알리기만 하고, 웹이 이어 올릴 파일이 같은지 맞춰 본다(다른 브라우저·PC에서도).
    #[serde(default)]
    #[schema(nullable = false)]
    pub fingerprint: Option<UploadFingerprint>,
}

/// 올리는 파일의 지문(이어 올리기 — 이름·고친 시각·앞부분 해시가 모두 같아야 같은 파일로 본다)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UploadFingerprint {
    /// 파일 이름
    pub file_name: String,
    /// 파일을 고친 시각(브라우저 `File.lastModified`, 유닉스 ms)
    pub last_modified: i64,
    /// 파일 앞 1 MiB(작으면 전체)의 SHA-256(16진 소문자)
    pub head_sha256: String,
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
    /// 업로드를 연(받는) 노드 — 다른 노드에 접속하면 보이지 않는다
    #[serde(default)]
    pub node: Uuid,
    /// 열 때 낸 파일 지문
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<UploadFingerprint>,
    /// 연 시각·마지막 조각 시각(유닉스 ms)
    pub created_at: u64,
    pub updated_at: u64,
    /// 조각이 더 오지 않으면 버리는 시각(유닉스 ms)
    pub expires_at: u64,
}

/// `POST /uploads/{id}/complete`(본문은 없어도 된다)
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CompleteUpload {
    /// 올린 파일 전체의 SHA-256(16진). 주면 서버가 받은 것과 맞춰 보고, 다르면 400 `upload_checksum_mismatch`(업로드를
    /// 버린다 — 처음부터 다시 올린다). 웹은 늘 준다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub sha256: Option<String>,
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

/// 바로 쓰는 업로드가 받는 풀 안 파일(끝내면 볼륨 파일 이름으로 바꾼다). 공유 풀에서 다른 노드의 업로드와 헷갈리지
/// 않게 노드 id를 넣는다(정리는 이 노드의 것만 본다).
fn direct_part(root: &FsPath, node: Uuid, id: Uuid) -> PathBuf {
    root.join(format!("{}{id}.raw.part", direct_prefix(node)))
}

fn direct_prefix(node: Uuid) -> String {
    format!(".upload-{node}.")
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

/// `f`(파일시스템 호출)를 blocking 스레드에서 [`FS_TIMEOUT`] 안에 돌린다. 멈춘 NFS에서 끝나지 않는 호출이 tokio 워커를
/// 붙잡지 않고, 시간이 지나면 `TimedOut`이다(끝나지 않은 호출은 blocking 스레드에 남는다).
pub(crate) async fn on_fs<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    on_fs_within(FS_TIMEOUT, f).await
}

async fn on_fs_within<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    match tokio::time::timeout(timeout, tokio::task::spawn_blocking(f)).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(std::io::Error::other(e)),
        Err(_) => Err(timed_out()),
    }
}

/// tokio 파일 호출(이미 blocking 스레드에서 돈다)에 [`FS_TIMEOUT`]을 둔다.
async fn timed<T>(fut: impl Future<Output = std::io::Result<T>>) -> std::io::Result<T> {
    tokio::time::timeout(FS_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| Err(timed_out()))
}

fn timed_out() -> std::io::Error {
    std::io::Error::new(ErrorKind::TimedOut, "the storage does not respond")
}

/// 파일 오류를 API 오류로: 응답 없음은 503 `pool_not_responding`, 공간 부족은 507, 그 밖은 500.
pub(crate) fn fs_error(e: std::io::Error) -> ApiError {
    match e.kind() {
        ErrorKind::TimedOut => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "pool_not_responding",
            "the storage that receives the upload does not respond; try again later",
        ),
        ErrorKind::StorageFull => ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            "not enough free space for the upload",
        ),
        _ => ApiError::internal(e),
    }
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

/// 받는 파일의 길이
async fn file_len(data: &FsPath) -> std::io::Result<u64> {
    let data = data.to_path_buf();
    on_fs(move || std::fs::metadata(data).map(|m| m.len())).await
}

/// 받는 자리가 보이는지: 바로 쓰는 업로드는 그 풀이 이 노드에 마운트돼 있고 루트가 응답할 때(`pools` — [`pool_roots`]),
/// 업로드 디렉터리는 그 디렉터리가 있을 때. 안 보이면 "받는 파일 없음"을 믿지 않는다(리뷰 77-8 I5).
async fn visible(record: &Record, pools: &PoolRoots) -> bool {
    let Some(parent) = record.data.parent().map(FsPath::to_path_buf) else {
        return false;
    };
    if record.upload.direct {
        match pools.get(&record.upload.pool) {
            // 마운트돼 있지 않거나 루트가 다른 데로 바뀌었다
            Some(root) if root.as_deref() != Some(parent.as_path()) => return false,
            // 풀이 있다 — 루트가 응답하는지 아래에서
            Some(_) => {}
            // 풀을 지웠다 — 디렉터리가 있으면 본다
            None => {}
        }
    }
    on_fs(move || std::fs::metadata(parent))
        .await
        .is_ok_and(|m| m.is_dir())
}

/// 풀 이름 → 이 노드에서 쓸 수 있는 파일 풀 루트(마운트되지 않은 NFS 풀·블록 풀은 `None`)
pub(crate) type PoolRoots = HashMap<String, Option<PathBuf>>;

pub(crate) fn pool_roots(cluster: &Cluster, mount_root: &FsPath) -> Option<PoolRoots> {
    let (_, pools) = cluster.list::<StoragePoolSpec>().ok()?;
    Some(
        pools
            .into_iter()
            .map(|p| {
                let root = check_pool_root(&p, mount_root).ok();
                (p.name, root)
            })
            .collect(),
    )
}

fn one_pool(state: &AppState, name: &str) -> PoolRoots {
    let mut pools = PoolRoots::new();
    if let Ok(Some(pool)) = state.cluster.get_by_name::<StoragePoolSpec>(name) {
        pools.insert(
            pool.name.clone(),
            check_pool_root(&pool, &state.mount_root).ok(),
        );
    }
    pools
}

/// 받은 바이트(받는 파일의 길이). 받는 파일이 없으면 404(끝내다 멈춤), 받는 풀이 안 보이거나 응답하지 않으면 503 —
/// 0으로 알리지 않는다(리뷰 77-8 I5).
async fn received(state: &AppState, record: &Record) -> Result<u64, ApiError> {
    match file_len(&record.data).await {
        Ok(len) => Ok(len),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            if visible(record, &one_pool(state, &record.upload.pool)).await {
                Err(not_found(record.upload.id))
            } else {
                Err(ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "pool_unavailable",
                    format!(
                        "storage pool {:?} that receives the upload is not available on this node now",
                        record.upload.pool
                    ),
                ))
            }
        }
        Err(e) => Err(fs_error(e)),
    }
}

/// 부른 사람의 살아 있는 업로드. 없거나 남의 것이면 404, 시간이 지났으면 버리고 404.
async fn own(state: &AppState, principal: &Principal, id: Uuid) -> Result<Record, ApiError> {
    let mut record = read_record(&state.upload_dir, id)
        .await
        .filter(|r| r.owner == principal.user)
        .ok_or_else(|| not_found(id))?;
    if record.upload.expires_at <= now_ms() {
        discard(state, &record).await;
        return Err(not_found(id));
    }
    record.upload.offset = received(state, &record).await?;
    Ok(record)
}

/// 받은 것과 기록을 지운다.
async fn discard(state: &AppState, record: &Record) {
    remove_data(&record.data).await;
    let _ = tokio::fs::remove_file(record_path(&state.upload_dir, record.upload.id)).await;
    state.uploads.forget(record.upload.id);
}

/// 받는 파일을 지운다(없어도 된다). 지웠거나 없으면 참.
async fn remove_data(data: &FsPath) -> bool {
    let data = data.to_path_buf();
    match on_fs(move || std::fs::remove_file(data)).await {
        Ok(()) => true,
        Err(e) => e.kind() == ErrorKind::NotFound,
    }
}

/// 조각을 쓰거나 끝내는 동안 같은 업로드의 다른 요청을 막는다(409 `upload_busy`). 쓰는 중에 버리기가 오면 놓을 때
/// 받은 것을 지운다([`Busy::release`], 요청이 끊겨 버려지면 drop이).
struct Busy {
    state: Arc<AppState>,
    id: Uuid,
    released: bool,
}

impl Busy {
    fn take(state: &Arc<AppState>, id: Uuid, completing: bool) -> Result<Self, ApiError> {
        let mut busy = state.uploads.busy.lock().unwrap();
        if busy.contains_key(&id) {
            return Err(busy_error(id));
        }
        busy.insert(
            id,
            Activity {
                completing,
                doomed: None,
            },
        );
        Ok(Self {
            state: state.clone(),
            id,
            released: false,
        })
    }

    /// 놓는다. 그동안 버리기가 왔으면 받은 것과 기록을 지우고 참.
    async fn release(mut self) -> bool {
        self.released = true;
        let doomed = self.state.uploads.busy.lock().unwrap().remove(&self.id);
        match doomed.and_then(|a| a.doomed) {
            Some(data) => {
                drop_doomed(&self.state, self.id, &data).await;
                true
            }
            None => false,
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let doomed = self.state.uploads.busy.lock().unwrap().remove(&self.id);
        if let Some(data) = doomed.and_then(|a| a.doomed) {
            let (state, id) = (self.state.clone(), self.id);
            tokio::spawn(async move { drop_doomed(&state, id, &data).await });
        }
    }
}

async fn drop_doomed(state: &AppState, id: Uuid, data: &FsPath) {
    tracing::info!(upload = %id, "removing an upload discarded while it was receiving");
    remove_data(data).await;
    let _ = tokio::fs::remove_file(record_path(&state.upload_dir, id)).await;
    state.uploads.forget(id);
}

fn busy_error(id: Uuid) -> ApiError {
    ApiError::conflict(
        "upload_busy",
        format!("upload {id} is receiving another request"),
    )
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
            let Some(pools) = pool_roots(&state.cluster, &state.mount_root) else {
                continue;
            };
            let busy = |id| state.uploads.is_busy(id);
            for id in sweep(&state.upload_dir, state.node, &pools, &busy, now_ms()).await {
                state.uploads.forget(id);
            }
        }
    });
}

/// 파일을 고친 시각(유닉스 ms)
fn modified_ms(m: &std::fs::Metadata) -> u64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64)
}

fn grace_passed(m: &std::fs::Metadata, now: u64) -> bool {
    modified_ms(m).saturating_add(ORPHAN_GRACE.as_millis() as u64) <= now
}

/// 버려진 업로드를 지우고 지운 id를 돌려준다. 노드가 켜질 때(Task 실행기의 고아 정리)와 [`SWEEP_EVERY`]마다 돈다.
/// - 시간이 지난 업로드: 받은 것과 기록을 함께 지운다. 받는 풀이 안 보이면 건너뛴다(기록을 남겨 다음에 다시).
/// - 받는 파일이 없는 기록(끝내다 멈춤): 받는 자리가 보이고 파일이 정말 없을 때(`NotFound`)만 지운다 — NFS가 잠깐 안
///   보이면 지우지 않는다(리뷰 77-8 I5). 쓰거나 끝내는 중(`busy`)인 것은 두지 않고 본다.
/// - 기록 없는 받는 파일(업로드 디렉터리의 `{id}.part`, 이 노드가 풀에 바로 쓰던 `.upload-{node}.{id}.raw.part`)과 깨진
///   기록·쓰다 만 기록: [`ORPHAN_GRACE`]가 지난 것을 지운다.
pub(crate) async fn sweep(
    upload_dir: &FsPath,
    node: Uuid,
    pools: &PoolRoots,
    busy: &(dyn Fn(Uuid) -> bool + Sync),
    now: u64,
) -> Vec<Uuid> {
    let mut removed = Vec::new();
    let sessions = sessions_dir(upload_dir);
    let Ok(mut entries) = tokio::fs::read_dir(&sessions).await else {
        return removed;
    };
    let mut known = HashSet::new();
    let mut parts = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = entry.metadata().await else {
            continue;
        };
        if name.ends_with(".json.tmp") {
            if grace_passed(&meta, now) {
                let _ = tokio::fs::remove_file(&path).await;
            }
            continue;
        }
        let Some(stem) = name.strip_suffix(".json") else {
            parts.push((name, path, meta));
            continue;
        };
        let record = tokio::fs::read(&path)
            .await
            .ok()
            .and_then(|t| serde_json::from_slice::<Record>(&t).ok());
        let Some(r) = record else {
            // 깨진 기록 — 받는 파일은 어디인지 모른다(풀 안 것은 아래 고아 정리가).
            if grace_passed(&meta, now) {
                let _ = tokio::fs::remove_file(&path).await;
            } else if let Ok(id) = Uuid::try_parse(stem) {
                known.insert(id);
            }
            continue;
        };
        let id = r.upload.id;
        known.insert(id);
        if busy(id) || !visible(&r, pools).await {
            continue;
        }
        let expired = r.upload.expires_at <= now;
        let lost = !expired
            && matches!(file_len(&r.data).await, Err(e) if e.kind() == ErrorKind::NotFound);
        if !(expired || lost) {
            continue;
        }
        tracing::info!(upload = %id, name = %r.upload.name, "removing an abandoned upload");
        if remove_data(&r.data).await {
            let _ = tokio::fs::remove_file(&path).await;
            known.remove(&id);
            removed.push(id);
        }
    }
    // 업로드 디렉터리의 기록 없는 받는 파일
    for (name, path, meta) in parts {
        let id = name
            .strip_suffix(".part")
            .and_then(|n| Uuid::try_parse(n).ok());
        if id.is_none_or(|id| !known.contains(&id)) && grace_passed(&meta, now) {
            let _ = tokio::fs::remove_file(&path).await;
        }
    }
    // 이 노드가 풀에 바로 쓰던 기록 없는 받는 파일(리뷰 77-8 I5 — 기록을 잃으면 아무도 지우지 않던 것)
    let prefix = direct_prefix(node);
    for root in pools.values().flatten() {
        let (root, prefix, known) = (root.clone(), prefix.clone(), known.clone());
        let orphans = on_fs(move || {
            let mut out = Vec::new();
            for entry in std::fs::read_dir(&root)?.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let id = name
                    .strip_prefix(&prefix)
                    .and_then(|n| n.strip_suffix(".raw.part"))
                    .and_then(|n| Uuid::try_parse(n).ok());
                if id.is_some_and(|id| !known.contains(&id))
                    && entry.metadata().is_ok_and(|m| grace_passed(&m, now))
                {
                    out.push(entry.path());
                }
            }
            Ok(out)
        })
        .await
        .unwrap_or_default();
        for path in orphans {
            tracing::info!(path = %path.display(), "removing an upload file without a record");
            remove_data(&path).await;
        }
    }
    removed
}

/// 업로드를 연다(201). 확인은 옛 업로드(`/volumes/import/check`)와 같고, 공간은 받을 자리(바로 쓰면 풀, 아니면 업로드
/// 디렉터리)에서 그 자리에 받는 중인 다른 업로드의 남은 양까지 더해 본다. 같은 이름의 업로드가 이미 열려 있으면 409
/// `name_taken`.
#[utoipa::path(post, path = "/uploads", tag = "storage",
    request_body = UploadRequest,
    responses((status = 201, body = Upload), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<UploadRequest>,
) -> Result<(StatusCode, Json<Upload>), ApiError> {
    start_sweeper(&state);
    if req.size == 0 {
        return Err(ApiError::bad_request("the file is empty"));
    }
    if let Some(f) = &req.fingerprint
        && (f.file_name.len() > 1024
            || f.head_sha256.len() != 64
            || !f.head_sha256.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(ApiError::bad_request(
            "fingerprint needs a file name of at most 1024 bytes and a hex SHA-256",
        ));
    }
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
    let _opening = state.uploads.opening.lock().await;
    let live = live_records(&state.upload_dir).await;
    if live.iter().any(|r| r.upload.name == q.name) {
        return Err(ApiError::conflict(
            "name_taken",
            format!("an upload of volume {:?} is in progress", q.name),
        ));
    }
    let id = Uuid::now_v7();
    let root = direct_root(&state, &pool, format, q.source_format);
    let data = match &root {
        Some(root) => direct_part(root, state.node, id),
        None => sessions.join(format!("{id}.part")),
    };
    let dir = data.parent().expect("a file in a directory").to_path_buf();
    // 같은 자리에 받는 중인 업로드의 남은 양을 예약으로 더한다(리뷰 77-8 I3 — 둘·셋이 각각 통과해 풀을 채우지 않게).
    let mut reserved = 0u64;
    for r in live
        .iter()
        .filter(|r| r.data.parent() == Some(dir.as_path()))
    {
        let got = file_len(&r.data).await.unwrap_or(0);
        reserved = reserved.saturating_add(r.upload.size.saturating_sub(got));
    }
    check_space(&dir, req.size.saturating_add(reserved)).await?;
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
            node: state.node,
            fingerprint: req.fingerprint,
            created_at: now,
            updated_at: now,
            expires_at: now + UPLOAD_IDLE.as_millis() as u64,
        },
        owner: principal.user,
        data,
    };
    let path = record.data.clone();
    on_fs(move || std::fs::File::create_new(path).map(drop))
        .await
        .map_err(fs_error)?;
    if let Err(e) = write_record(&state.upload_dir, &record).await {
        remove_data(&record.data).await;
        return Err(e);
    }
    audit.detail(format!(
        "volume upload {} to pool {} ({} bytes)",
        record.upload.name, record.upload.pool, record.upload.size
    ));
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

/// 부른 사람이 이 노드에 연 업로드(연 차례). 탭을 닫았다 와도 같은 파일(지문)을 고르면 잇는 데 쓴다. 받는 풀이 안 보이는
/// 업로드는 받은 양을 모르므로 빼고 알린다.
#[utoipa::path(get, path = "/uploads", tag = "storage",
    responses((status = 200, body = UploadList), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_uploads(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<UploadList>, ApiError> {
    let mut items = Vec::new();
    for mut record in live_records(&state.upload_dir).await {
        if record.owner != principal.user {
            continue;
        }
        match received(&state, &record).await {
            Ok(offset) => {
                record.upload.offset = offset;
                items.push(record.upload);
            }
            Err(e) => {
                tracing::warn!(upload = %record.upload.id, error = %e.message, "cannot read an upload");
            }
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
/// 쓰는 동안 버리기(`DELETE`)가 오면 이 조각을 마친 뒤 지우고 404. 감사 기록에 적지 않는다(열기·끝내기·버리기만).
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
    let busy = Busy::take(&state, id, false)?;
    let result = write_chunk(&state, &principal, id, q.offset, length, body).await;
    if busy.release().await {
        return Err(ApiError::not_found(format!("upload {id} was discarded")));
    }
    result.map(Json)
}

async fn write_chunk(
    state: &AppState,
    principal: &Principal,
    id: Uuid,
    want: u64,
    length: u64,
    body: Body,
) -> Result<Upload, ApiError> {
    let mut record = own(state, principal, id).await?;
    // 연 뒤에 풀 권한을 거두면 더 받지 않는다(리뷰 77-8 S4).
    let pool = find_pool(state, &record.upload.pool)?;
    state
        .authz
        .require(principal, Privilege::StorageAllocate, &pool)?;
    let offset = record.upload.offset;
    if want != offset {
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
    // 받는 자리(풀이면 실행 중 VM 디스크가 함께 쓴다)에 이 조각을 받고도 여유가 남는지 조각마다 본다(리뷰 77-8 I3).
    let dir = record
        .data
        .parent()
        .expect("a file in a directory")
        .to_path_buf();
    let available = on_fs(move || fs4::available_space(dir))
        .await
        .map_err(fs_error)?;
    if length.saturating_add(UPLOAD_RESERVE_BYTES) > available {
        return Err(fs_error(ErrorKind::StorageFull.into()));
    }
    let mut hash = state.uploads.take_hash(id, offset);
    let mut file = timed(tokio::fs::OpenOptions::new().write(true).open(&record.data))
        .await
        .map_err(|e| match e.kind() {
            ErrorKind::NotFound => not_found(id),
            _ => fs_error(e),
        })?;
    timed(file.seek(SeekFrom::Start(offset)))
        .await
        .map_err(fs_error)?;
    let mut got = 0u64;
    // 쓰고 해시에 더한 바이트(받았지만 쓰지 못한 조각은 빼고)
    let mut written = 0u64;
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
            if let Err(e) = timed(file.write_all(&chunk)).await {
                // 얼마나 썼는지 모른다 — 해시는 끝낼 때 파일에서 다시 센다.
                hash = None;
                return Err(fs_error(e));
            }
            if let Some(h) = &mut hash {
                h.update(&chunk);
            }
            written += chunk.len() as u64;
        }
        Ok(())
    }
    .await;
    // 받은 것은 남긴다(이어 올리기). 디스크에 닿은 뒤에 받은 데로 알린다.
    timed(file.sync_data()).await.map_err(fs_error)?;
    if let Some(h) = hash {
        state.uploads.keep_hash(id, offset + written, h);
    }
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
    Ok(record.upload)
}

/// 받은 파일 전체의 SHA-256(16진) — 받으며 세지 못했을 때(노드를 다시 켬·쓰다 실패) 읽어서 센다.
async fn hash_file(path: &FsPath) -> Result<String, ApiError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        Ok::<_, std::io::Error>(hex::encode(hash.finalize()))
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(fs_error)
}

/// 받은 파일을 `source`(볼륨 파일·변환할 원본)로 옮기고 `then`(Task 만들기)을 부른다. `then`이 실패하면 파일을 받은
/// 자리로 되돌린다(리뷰 77-8 I8 — 다 받은 데이터를 지우지 않고 다시 끝낼 수 있게).
async fn hand_over<T>(
    data: &FsPath,
    source: &FsPath,
    then: impl Future<Output = Result<T, ApiError>>,
) -> Result<T, ApiError> {
    timed(tokio::fs::rename(data, source))
        .await
        .map_err(fs_error)?;
    let result = then.await;
    if result.is_err()
        && let Err(e) = timed(tokio::fs::rename(source, data)).await
    {
        tracing::warn!(path = %source.display(), error = %e, "cannot put a finished upload back");
    }
    result
}

/// 다 받은 업로드로 볼륨을 만들고 가져오기 Task를 돌려준다(202). 바로 쓴 업로드는 받은 파일을 볼륨 파일 이름으로
/// 바꾸고(Task는 확인만), 아니면 Task가 변환한다. 덜 받았으면 409 `upload_incomplete`. 권한·이름·풀은 다시 본다.
/// `sha256`을 주면 받은 것과 맞춰 보고 다르면 400 `upload_checksum_mismatch`(업로드를 버린다). Task를 만들지 못하면
/// 받은 것은 그대로 남아 다시 끝낼 수 있다.
#[utoipa::path(post, path = "/uploads/{id}/complete", tag = "storage",
    params(("id" = Uuid, Path)),
    request_body(content = Option<CompleteUpload>),
    responses((status = 202, body = Resource<orbivisor_types::task::TaskSpec, orbivisor_types::task::TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn complete_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<Uuid>,
    req: Option<Json<Option<CompleteUpload>>>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let req = req.and_then(|Json(r)| r).unwrap_or_default();
    let _busy = Busy::take(&state, id, true)?;
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
    let checked = match &req.sha256 {
        Some(want) => {
            let got = match state.uploads.finished_hash(id, u.size) {
                Some(got) => got,
                None => hash_file(&record.data).await?,
            };
            if !got.eq_ignore_ascii_case(want.trim()) {
                discard(&state, &record).await;
                audit.detail(format!(
                    "volume upload {} discarded: SHA-256 {got} differs from {want}",
                    u.name
                ));
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "upload_checksum_mismatch",
                    format!(
                        "the received file (SHA-256 {got}) differs from the one sent ({want}); \
                         the upload was discarded — upload the file again"
                    ),
                ));
            }
            format!(", sha256 {got}")
        }
        None => String::new(),
    };
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
    // 여기서부터 실패하면 볼륨을 지우고 받은 파일을 받은 자리로 되돌린다(요청이 끊겨 버려져도).
    let guard = UploadGuard {
        state: state.clone(),
        volume: volume.id,
        source: source.clone(),
        restore: Some(record.data.clone()),
        armed: true,
    };
    let op = TaskOp::VolumeImport {
        source: source.clone(),
        source_format: q.source_format,
    };
    let made = hand_over(
        &record.data,
        &source,
        create_task(&state.cluster, op, volume.id, state.node),
    )
    .await;
    let task = match made {
        Ok(task) => task,
        Err(e) => {
            guard.abandon().await;
            return Err(e);
        }
    };
    let mut guard = guard;
    guard.armed = false;
    let _ = tokio::fs::remove_file(record_path(&state.upload_dir, id)).await;
    state.uploads.forget(id);
    audit.detail(format!(
        "volume upload {} to pool {} complete ({} bytes{checked})",
        record.upload.name, record.upload.pool, record.upload.size
    ));
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 업로드를 버린다(204, 받은 것도 지운다). 조각을 쓰는 중이면 그 조각이 끝난 뒤 지운다(이름·공간 예약은 바로 풀린다).
/// 끝내는 중이면 409 `upload_busy`.
#[utoipa::path(delete, path = "/uploads/{id}", tag = "storage",
    params(("id" = Uuid, Path)),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_upload(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let record = read_record(&state.upload_dir, id)
        .await
        .filter(|r| r.owner == principal.user)
        .ok_or_else(|| not_found(id))?;
    audit.detail(format!("volume upload {} discarded", record.upload.name));
    let taken = {
        let mut busy = state.uploads.busy.lock().unwrap();
        match busy.get_mut(&id) {
            Some(a) if a.completing => return Err(busy_error(id)),
            Some(a) => {
                a.doomed = Some(record.data.clone());
                false
            }
            None => {
                busy.insert(id, Activity::default());
                true
            }
        }
    };
    if taken {
        let busy = Busy {
            state: state.clone(),
            id,
            released: false,
        };
        discard(&state, &record).await;
        busy.release().await;
    } else {
        // 쓰는 쪽이 놓을 때 받은 것을 지운다. 기록은 지금 지워 이름·공간 예약을 푼다.
        let _ = tokio::fs::remove_file(record_path(&state.upload_dir, id)).await;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: Uuid, expires_at: u64, data: PathBuf, direct: bool) -> Record {
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
                direct,
                node: Uuid::nil(),
                fingerprint: None,
                created_at: 0,
                updated_at: 0,
                expires_at,
            },
            owner: Uuid::nil(),
            data,
        }
    }

    fn write(dir: &FsPath, r: &Record, data: bool) {
        if data {
            std::fs::write(&r.data, b"x").unwrap();
        }
        std::fs::write(
            record_path(dir, r.upload.id),
            serde_json::to_vec(r).unwrap(),
        )
        .unwrap();
    }

    fn names(dir: &FsPath) -> Vec<String> {
        let mut left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        left
    }

    /// 정리 시각: 방금 만든 파일이 [`ORPHAN_GRACE`]를 지난 때
    fn later() -> u64 {
        now_ms() + 2 * ORPHAN_GRACE.as_millis() as u64
    }

    /// 시간이 지난 업로드, 받는 파일이 사라진 기록, 기록 없는 받는 파일·깨진 기록·쓰다 만 기록을 지우고 살아 있는 것과
    /// 쓰는 중인 것은 둔다. 방금 생긴(여는 중일 수 있는) 고아는 두었다 지운다.
    #[tokio::test]
    async fn sweep_removes_abandoned_uploads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let sessions = sessions_dir(dir);
        std::fs::create_dir_all(&sessions).unwrap();
        let now = later();
        let [live, old, lost, busy, orphan] = std::array::from_fn(|_| Uuid::now_v7());
        for (id, expires, data) in [
            (live, now + 1, true),
            (old, now - 1, true),
            (lost, now + 1, false),
            (busy, now + 1, false),
        ] {
            write(
                dir,
                &record(id, expires, sessions.join(format!("{id}.part")), false),
                data,
            );
        }
        std::fs::write(sessions.join(format!("{orphan}.part")), b"x").unwrap();
        std::fs::write(sessions.join("x.json.tmp"), b"{").unwrap();
        std::fs::write(sessions.join("junk.json"), b"not json").unwrap();

        // 방금 생긴 고아·쓰다 만 기록은 아직 둔다(여는 중일 수 있다).
        let pools = PoolRoots::new();
        let removed = sweep(dir, Uuid::nil(), &pools, &|id| id == busy, now_ms()).await;
        assert_eq!(removed, [lost]);
        let left = names(&sessions);
        for name in [
            format!("{orphan}.part"),
            "x.json.tmp".into(),
            "junk.json".into(),
        ] {
            assert!(left.contains(&name), "{left:?}");
        }

        let removed = sweep(dir, Uuid::nil(), &pools, &|id| id == busy, now).await;
        assert_eq!(removed, [old]);
        assert_eq!(
            names(&sessions),
            [
                format!("{live}.json"),
                format!("{live}.part"),
                format!("{busy}.json")
            ]
        );
        // 업로드 디렉터리가 없어도 된다.
        sweep(&dir.join("missing"), Uuid::nil(), &pools, &|_| false, 0).await;
    }

    /// 바로 쓰는 업로드: 풀이 마운트되지 않았으면(NFS가 잠깐 안 보임) 받는 파일이 없어도·시간이 지나도 지우지 않는다. 풀이
    /// 보일 때만 지우고, 풀 루트의 이 노드의 기록 없는 받는 파일도 지운다(다른 노드의 것·방금 생긴 것은 둔다).
    #[tokio::test]
    async fn sweep_leaves_uploads_on_a_pool_it_cannot_see() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("uploads");
        let root = tmp.path().join("pool");
        std::fs::create_dir_all(sessions_dir(&dir)).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let node = Uuid::now_v7();
        let now = later();
        let [lost, old, orphan] = std::array::from_fn(|_| Uuid::now_v7());
        write(
            &dir,
            &record(lost, now + 1, direct_part(&root, node, lost), true),
            false,
        );
        write(
            &dir,
            &record(old, now - 1, direct_part(&root, node, old), true),
            true,
        );
        let mine = direct_part(&root, node, orphan);
        let theirs = direct_part(&root, Uuid::now_v7(), orphan);
        std::fs::write(&mine, b"x").unwrap();
        std::fs::write(&theirs, b"x").unwrap();
        let unmounted = PoolRoots::from([("local".to_string(), None)]);
        assert!(
            sweep(&dir, node, &unmounted, &|_| false, now)
                .await
                .is_empty()
        );
        assert_eq!(names(&sessions_dir(&dir)).len(), 2);
        assert_eq!(names(&root).len(), 3);

        // 루트가 다른 데를 가리키는 풀도 안 보이는 것으로 본다.
        let elsewhere = PoolRoots::from([("local".to_string(), Some(tmp.path().join("x")))]);
        assert!(
            sweep(&dir, node, &elsewhere, &|_| false, now)
                .await
                .is_empty()
        );

        let mounted = PoolRoots::from([("local".to_string(), Some(root.clone()))]);
        let mut removed = sweep(&dir, node, &mounted, &|_| false, now).await;
        removed.sort();
        let mut want = vec![lost, old];
        want.sort();
        assert_eq!(removed, want);
        assert!(names(&sessions_dir(&dir)).is_empty());
        assert_eq!(
            names(&root),
            [theirs.file_name().unwrap().to_str().unwrap()]
        );
    }

    /// 멈춘 파일시스템 호출은 기다리지 않는다.
    #[tokio::test]
    async fn filesystem_calls_time_out() {
        let slow = on_fs_within(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_millis(500));
            Ok(())
        })
        .await;
        assert_eq!(slow.unwrap_err().kind(), ErrorKind::TimedOut);
        assert_eq!(
            fs_error(timed_out()).status,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(on_fs(|| Ok(7)).await.unwrap(), 7);
    }

    /// 끝내기에서 Task를 만들지 못하면 받은 파일을 받은 자리로 되돌린다(다시 끝낼 수 있다). 되면 옮긴 채 둔다.
    #[tokio::test]
    async fn completing_puts_the_file_back_when_the_task_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let (data, source) = (tmp.path().join("u.part"), tmp.path().join("v.raw"));
        std::fs::write(&data, b"all of it").unwrap();
        let failed: Result<(), _> = hand_over(&data, &source, async {
            Err(ApiError::internal("raft is busy"))
        })
        .await;
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&data).unwrap(), b"all of it");
        assert!(!source.exists());

        assert_eq!(hand_over(&data, &source, async { Ok(1) }).await.unwrap(), 1);
        assert!(!data.exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"all of it");
    }

    /// 받으며 센 해시는 받은 데가 맞을 때만 잇는다(어긋나면 끝낼 때 파일을 읽어 센다).
    #[test]
    fn running_hashes_follow_the_offset() {
        let uploads = Uploads::default();
        let id = Uuid::now_v7();
        let mut h = uploads.take_hash(id, 0).unwrap();
        h.update(b"abc");
        uploads.keep_hash(id, 3, h);
        assert_eq!(uploads.finished_hash(id, 4), None);
        assert_eq!(
            uploads.finished_hash(id, 3).unwrap(),
            hex::encode(Sha256::digest(b"abc"))
        );
        assert!(uploads.take_hash(id, 2).is_none());
        // 꺼낸 뒤 어긋났으면 처음(0)부터만 다시 센다.
        assert!(uploads.take_hash(id, 3).is_none());
        assert!(uploads.take_hash(id, 0).is_some());
    }

    #[test]
    fn records_keep_the_upload_flat() {
        let r = record(Uuid::nil(), 5, "d".into(), false);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["name"], "disk");
        assert_eq!(json["owner"], Uuid::nil().to_string());
        assert!(json.get("fingerprint").is_none());
        let back: Record = serde_json::from_value(json).unwrap();
        assert_eq!(back.upload, r.upload);
    }
}
