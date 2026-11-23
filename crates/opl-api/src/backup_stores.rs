//! 백업 저장소 `/api/v1/backup-stores`(계획 68 설계 §6·§9.4·§12.6·§13·§14): 만들기(초기화 Task)·바꾸기·지우기, GC·검증·
//! scan Task, 연결 확인, 키 내보내기·escrow, 넘겨받기. 저장소는 루트 종류다 — 읽기는 `system.read`, 바꾸기는
//! `storage.config`, 키·넘겨받기는 `cluster.admin`(경로표). 보기·watch·감사에는 S3 비밀 키와 주 키가 없다
//! ([`BackupStoreView`] — 지문과 접근 키 id만).
//!
//! API 노드가 저장소에 곧바로 닿는 것은 키를 다루는 일뿐이다: 만들 때의 확인(닿으면 — 지문·주인 표식), escrow 쓰기·
//! 풀기, 연결 확인, 넘겨받기. 그 밖의 일(초기화·GC·검증·scan)은 저장소를 쓸 수 있는 노드의 Task가 한다.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_store::backend::Backend;
use opl_store::crypto::{EscrowFile, KeyError, MasterKey, StoreKeys, escrow_key};
use opl_store::format::OwnerFile;
use opl_store::{Store, StoreError};
use opl_types::backup::BackupSpec;
use opl_types::backup_job::{JobSchedule, next_run};
use opl_types::backup_store::{
    BackupStore, BackupStoreSpec, BackupStoreStatus, StoreBackend, StoreEncryption, StoreOwner,
    StorePhase, VerifyPolicy,
};
use opl_types::node::BACKUP_STORE;
use opl_types::resource::Resource;
use opl_types::storage::StoragePoolSpec;
use opl_types::task::{Task, TaskOp, TaskSpec};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::backups::now;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::resources::{List, check_name, lookup};
use crate::state::AppState;
use crate::stores;
use crate::tasks::create_task;

/// API 노드가 저장소에 닿아 보는 일(확인·escrow·넘겨받기)의 시간 제한
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// 주인의 하트비트가 이보다 오래됐으면 `force` 없이 넘겨받는다(§12.6).
const TAKE_OVER_AFTER: u64 = 24 * 3600;

// ---- 보기 ----

/// 보기의 S3 대상: 비밀 키가 없다.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct S3TargetView {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub path_style: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub ca_pem: Option<String>,
    pub access_key_id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub insecure_http: bool,
}

/// 보기의 백엔드(`{"pool": "bk"}` 또는 `{"s3": {...}}`)
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StoreBackendView {
    Pool(String),
    S3(S3TargetView),
}

/// 보기의 암호화: 키 지문만
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StoreEncryptionView {
    pub fingerprint: String,
}

/// 비밀(S3 비밀 키·주 키)을 뺀 저장소 스펙
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupStoreSpecView {
    pub backend: StoreBackendView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub encryption: Option<StoreEncryptionView>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    pub gc: JobSchedule,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub verify: Option<VerifyPolicy>,
    pub max_streams: u8,
}

/// 응답의 저장소
pub type BackupStoreView = Resource<BackupStoreSpecView, BackupStoreStatus>;

/// 저장소 스펙 JSON에서 비밀(`encryption.key`, `backend.s3.secret_access_key`)을 뺀다. 보기와 watch 이벤트가 쓴다.
pub(crate) fn redact_json(spec: &mut Value) {
    if let Some(e) = spec.get_mut("encryption").and_then(Value::as_object_mut) {
        e.remove("key");
    }
    if let Some(s3) = spec
        .pointer_mut("/backend/s3")
        .and_then(Value::as_object_mut)
    {
        s3.remove("secret_access_key");
    }
}

/// 비밀을 뺀 보기
pub(crate) fn view(store: &BackupStore) -> Result<BackupStoreView, ApiError> {
    let mut v = serde_json::to_value(store).map_err(ApiError::internal)?;
    redact_json(&mut v["spec"]);
    serde_json::from_value(v).map_err(ApiError::internal)
}

// ---- 요청 ----

/// 만들기의 스펙(암호화는 [`CreateStoreRequest::encryption`]으로 따로 — 키를 본문에 받지 않는다)
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreSpecRequest {
    pub backend: StoreBackend,
    /// 다른 클러스터가 주인인 저장소를 복원용으로 붙인다(백업·지우기·GC 막힘).
    #[serde(default)]
    pub read_only: bool,
    /// GC 일정(UTC). 없으면 매일 05:00.
    #[serde(default)]
    #[schema(nullable = false)]
    pub gc: Option<JobSchedule>,
    /// 예약 검증. 없으면 예약 검증을 하지 않는다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub verify: Option<VerifyPolicy>,
    /// S3 동시 요청 수(1~64, 기본 8)
    #[serde(default)]
    #[schema(nullable = false)]
    pub max_streams: Option<u8>,
    /// 읽기 전용으로 붙이는 백업 풀 안 저장소의 id(`{풀 루트}/store/<id>/` — 다른 클러스터가 만든 디렉터리). S3는 쓰지
    /// 않는다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub store_id: Option<Uuid>,
}

/// 만들기의 암호화: 셋 가운데 하나 — 새 키(`generate`), 복구 키 가져오기, 저장소 안 escrow를 암호 문구로 풀기
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EncryptionRequest {
    #[serde(default)]
    pub generate: bool,
    /// 복구 키(`OPK1-…`)
    #[serde(default)]
    #[schema(nullable = false)]
    pub recovery_key: Option<String>,
    /// 저장소 `keys/<지문>.json`을 풀 암호 문구
    #[serde(default)]
    #[schema(nullable = false)]
    pub escrow_passphrase: Option<String>,
}

impl std::fmt::Debug for EncryptionRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionRequest")
            .field("generate", &self.generate)
            .field("recovery_key", &self.recovery_key.as_ref().map(|_| "***"))
            .field(
                "escrow_passphrase",
                &self.escrow_passphrase.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// 저장소 만들기 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateStoreRequest {
    pub name: String,
    pub spec: StoreSpecRequest,
    /// 없으면 평문 저장소
    #[serde(default)]
    #[schema(nullable = false)]
    pub encryption: Option<EncryptionRequest>,
    /// 만들 때 주 키를 이 암호 문구로 감싸 저장소 안에 둔다(escrow — 클러스터를 잃어도 연다). 암호 문구는 저장하지
    /// 않는다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub escrow_passphrase: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

impl std::fmt::Debug for CreateStoreRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateStoreRequest")
            .field("name", &self.name)
            .field("spec", &self.spec)
            .field("encryption", &self.encryption)
            .field(
                "escrow_passphrase",
                &self.escrow_passphrase.as_ref().map(|_| "***"),
            )
            .finish()
    }
}

/// 만든 저장소
#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedStore {
    pub store: Resource<BackupStoreSpecView, BackupStoreStatus>,
    /// 새 키를 만들었으면 그 복구 키(이때 한 번 — 나중에는 `key/export`). 잃으면 백업을 되살릴 길이 없다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub recovery_key: Option<String>,
    /// 초기화 Task
    pub task: Task,
}

/// S3 자격 증명 바꾸기(둘 다)
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"***")
            .finish()
    }
}

/// 저장소에서 바꿀 값. 주어진 것만 바꾼다. 백엔드·암호화·읽기 전용은 바꾸지 못한다(읽기 전용은 `take-over`).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StorePatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub gc: Option<JobSchedule>,
    /// 예약 검증. `null`이면 끈다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<VerifyPolicy>, nullable)]
    pub verify: Option<Option<VerifyPolicy>>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub max_streams: Option<u8>,
    /// S3 저장소만
    #[serde(default)]
    #[schema(nullable = false)]
    pub s3_credentials: Option<S3Credentials>,
    /// S3 저장소만. `null`이면 시스템 루트로 돌아간다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub ca_pem: Option<Option<String>>,
}

/// 감싸 둘 암호 문구
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EscrowRequest {
    pub passphrase: String,
}

/// 복구 키
#[derive(Serialize, ToSchema)]
pub struct RecoveryKey {
    pub recovery_key: String,
}

/// 넘겨받기 본문
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TakeOverRequest {
    /// 주인의 하트비트가 24시간 안이어도 넘겨받는다(옛 주인이 아직 살아 있으면 두 클러스터가 서로의 백업을 지울 수 있다).
    #[serde(default)]
    pub force: bool,
}

/// 노드 하나가 저장소에 닿는지(노드 보고 `NodeStatus.backup_stores`)
#[derive(Debug, Serialize, ToSchema)]
pub struct StoreNodeCheck {
    pub node: Uuid,
    pub name: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<String>,
    pub checked_at: u64,
}

/// 연결 확인 결과: 이 노드에서 곧바로 읽어 본 것과 노드마다 보고한 것
#[derive(Debug, Serialize, ToSchema)]
pub struct StoreCheck {
    /// API 노드에서 `store.json`을 읽고 키가 맞았다.
    pub ok: bool,
    /// API 노드가 그 저장소를 쓰지 못하면(다른 노드의 로컬 풀) 확인하지 않았다.
    pub checked_here: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<String>,
    pub nodes: Vec<StoreNodeCheck>,
}

// ---- 도움 ----

fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("invalid_backup_store")
}

/// API 노드에서 저장소 일이 실패한 것: 닿지 않음은 503 `store_unavailable`, 키가 맞지 않음은 400 `store_key_mismatch`.
fn store_failed(name: &str, e: StoreError) -> ApiError {
    match e {
        StoreError::Unavailable(_) | StoreError::Io(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "store_unavailable",
            stores::store_error(name, e),
        ),
        StoreError::KeyMismatch(_) => {
            ApiError::bad_request(stores::store_error(name, e)).with_code("store_key_mismatch")
        }
        _ => ApiError::bad_request(stores::store_error(name, e)),
    }
}

fn key_failed(e: KeyError) -> ApiError {
    match e {
        KeyError::BadRecoveryKey | KeyError::BadKey => ApiError::bad_request(e.to_string()),
        _ => ApiError::bad_request(e.to_string()).with_code("store_key_mismatch"),
    }
}

fn timed_out(name: &str) -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "store_unavailable",
        format!("backup store {name:?}: timed out reaching the store"),
    )
}

/// 이 노드에서 저장소의 백엔드. 이 노드가 쓸 수 없으면 409 `store_not_on_node`.
fn backend_here(state: &AppState, store: &BackupStore) -> Result<Arc<dyn Backend>, ApiError> {
    stores::open_backend(&state.cluster, store, state.node, &state.mount_root)
        .map_err(|e| ApiError::conflict("store_not_on_node", e))
}

/// 이 노드에서 저장소를 연다(키 지문 확인). 쓸 수 없으면 409 `store_not_on_node`, 닿지 않으면 503.
async fn open_here(state: &AppState, store: &BackupStore) -> Result<Store, ApiError> {
    let backend = backend_here(state, store)?;
    let key = stores::master_key(store).map_err(ApiError::internal)?;
    tokio::time::timeout(PROBE_TIMEOUT, Store::open(backend, key.as_ref()))
        .await
        .map_err(|_| timed_out(&store.name))?
        .map_err(|e| store_failed(&store.name, e))
}

/// 이 클러스터의 이름과 CA 지문(주인 표식)
fn this_cluster(state: &AppState) -> Result<(String, String), ApiError> {
    let info = crate::cluster::cluster_info(state)?;
    let fp = opl_peer::pki::fingerprint(&info.spec.ca_cert_pem).map_err(ApiError::internal)?;
    Ok((info.name, fp))
}

/// 저장소 안 escrow를 암호 문구로 풀어 주 키를 얻는다.
async fn unlock_escrow(
    name: &str,
    backend: &dyn Backend,
    passphrase: String,
) -> Result<MasterKey, ApiError> {
    let failed = |e| store_failed(name, e);
    let info = opl_store::store::read_info(backend)
        .await
        .map_err(failed)?
        .ok_or_else(|| {
            invalid(format!(
                "backup store {name:?}: there is no backup store at this location"
            ))
        })?;
    let fp = info
        .fingerprint()
        .map(str::to_string)
        .ok_or_else(|| ApiError::bad_request(format!("backup store {name:?} is not encrypted")))?;
    let raw = match backend.get(&escrow_key(&fp)).await {
        Ok(raw) => raw,
        Err(StoreError::NotFound(_)) => {
            return Err(ApiError::bad_request(format!(
                "backup store {name:?} holds no escrowed key; use the recovery key"
            ))
            .with_code("store_key_required"));
        }
        Err(e) => return Err(failed(e)),
    };
    let file: EscrowFile = serde_json::from_slice(&raw)
        .map_err(|e| invalid(format!("the escrowed key file is not valid: {e}")))?;
    tokio::task::spawn_blocking(move || opl_store::crypto::escrow_unwrap(&file, &passphrase))
        .await
        .map_err(ApiError::internal)?
        .map_err(key_failed)
}

/// 주 키를 암호 문구로 감싸 저장소 `keys/<지문>.json`에 쓴다.
async fn write_escrow(
    name: &str,
    backend: &dyn Backend,
    mk: &MasterKey,
    passphrase: String,
) -> Result<(), ApiError> {
    if passphrase.len() < 8 {
        return Err(ApiError::bad_request(
            "the escrow passphrase must be at least 8 characters",
        ));
    }
    let key = mk.clone();
    let file =
        tokio::task::spawn_blocking(move || opl_store::crypto::escrow_wrap(&key, &passphrase))
            .await
            .map_err(ApiError::internal)?
            .map_err(key_failed)?;
    let bytes = serde_json::to_vec_pretty(&file).map_err(ApiError::internal)?;
    tokio::time::timeout(
        PROBE_TIMEOUT,
        backend.put(&escrow_key(&file.fingerprint), bytes),
    )
    .await
    .map_err(|_| timed_out(name))?
    .map_err(|e| store_failed(name, e))
}

/// 저장소 Task를 돌릴 노드(이 노드를 먼저). 없으면 409 `no_node`.
fn task_node(state: &AppState, store: &BackupStore) -> Result<Uuid, ApiError> {
    stores::task_node(&state.cluster, store, Some(state.node))
        .map_err(ApiError::bad_request)?
        .ok_or_else(|| {
            ApiError::conflict(
                "no_node",
                format!("no online node can use backup store {:?}", store.name),
            )
        })
}

/// 저장소를 대상으로 끝나지 않은 Task(같은 종류면 409 `store_busy`)
fn check_not_running(state: &AppState, store: &BackupStore, op: &TaskOp) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if tasks.iter().any(|t| {
        t.spec.target == store.id
            && !t.status.state.is_finished()
            && std::mem::discriminant(&t.spec.op) == std::mem::discriminant(op)
    }) {
        return Err(ApiError::conflict(
            "store_busy",
            format!(
                "backup store {:?} is already running this task; try again when it has finished",
                store.name
            ),
        ));
    }
    Ok(())
}

fn ready(store: &BackupStore) -> Result<(), ApiError> {
    crate::backups::check_store(store, false)
}

fn backend_text(spec: &BackupStoreSpec) -> String {
    match &spec.backend {
        StoreBackend::Pool(p) => format!("pool {p}"),
        StoreBackend::S3(s3) => format!(
            "s3 {}/{}{}",
            s3.endpoint,
            s3.bucket,
            s3.prefix
                .as_deref()
                .map(|p| format!("/{}", p.trim_end_matches('/')))
                .unwrap_or_default()
        ),
    }
}

// ---- 핸들러 ----

#[utoipa::path(get, path = "/backup-stores", tag = "backup-stores",
    responses((status = 200, body = List<Resource<BackupStoreSpecView, BackupStoreStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_stores(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
) -> Result<Json<List<BackupStoreView>>, ApiError> {
    let (revision, stores) = state.cluster.list::<BackupStoreSpec>()?;
    let items = stores.iter().map(view).collect::<Result<_, _>>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/backup-stores/{key}", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<BackupStoreSpecView, BackupStoreStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<BackupStoreView>, ApiError> {
    Ok(Json(view(&lookup::<BackupStoreSpec>(
        &state.cluster,
        &key,
    )?)?))
}

/// 저장소를 만들고 초기화 Task를 돌려준다(201 — 저장소는 `initializing`). 쓰기 저장소는 `store.json`·주인 표식을 쓰고,
/// 읽기 전용은 있는 저장소를 키로 열어 본다. 암호화는 새 키(`generate` — 응답에 복구 키가 한 번 들어 있다), 복구 키,
/// 저장소 안 escrow 가운데 하나다. API 노드가 저장소에 닿으면 미리 확인한다: 키 지문이 다르면 400 `store_key_mismatch`,
/// 암호화 저장소인데 키가 없으면 400 `store_key_required`, 다른 클러스터가 주인인 곳에 쓰기 저장소면 409
/// `store_owned_elsewhere`. 같은 풀·버킷/접두사의 저장소가 있으면 409 `store_target_in_use`, 형식이 틀리면 400
/// `invalid_backup_store`, 옛 노드가 있으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/backup-stores", tag = "backup-stores",
    request_body = CreateStoreRequest,
    responses((status = 201, body = CreatedStore), (status = "4XX", body = crate::error::ErrorBody),
              (status = 503, body = crate::error::ErrorBody)))]
pub async fn create_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateStoreRequest>,
) -> Result<(StatusCode, Json<CreatedStore>), ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    check_name(&req.name)?;
    let r = req.spec;
    let mut spec: BackupStoreSpec =
        serde_json::from_value(serde_json::json!({ "backend": r.backend })).map_err(invalid)?;
    spec.read_only = r.read_only;
    if let Some(gc) = r.gc {
        spec.gc = gc;
    }
    spec.verify = r.verify;
    if let Some(n) = r.max_streams {
        spec.max_streams = n;
    }
    spec.gc.days.sort();
    if let Some(v) = spec.verify.as_mut() {
        v.schedule.days.sort();
    }
    spec.validate().map_err(invalid)?;
    if r.store_id.is_some() && !(spec.read_only && matches!(spec.backend, StoreBackend::Pool(_))) {
        return Err(invalid(
            "spec.store_id is only for attaching a store in a backup pool read-only",
        ));
    }
    if let StoreBackend::Pool(pool) = &spec.backend {
        let p = state
            .cluster
            .get_by_name::<StoragePoolSpec>(pool)?
            .ok_or_else(|| invalid(format!("storage pool {pool:?} not found")))?;
        if !p.spec.backups {
            return Err(invalid(format!(
                "storage pool {pool:?} does not store backups; turn on its backups setting"
            )));
        }
    }
    // 같은 곳의 저장소(Raft도 막지만 저장소에 닿아 보기 전에 알린다)
    let target = opl_types::backup_store::store_target_key(&spec.backend);
    let (_, existing) = state.cluster.list::<BackupStoreSpec>()?;
    if let Some(other) = existing.iter().find(|s| {
        target.is_some() && opl_types::backup_store::store_target_key(&s.spec.backend) == target
    }) {
        return Err(ApiError::conflict(
            "store_target_in_use",
            format!(
                "backup store {:?} already uses {}",
                other.name,
                backend_text(&spec)
            ),
        ));
    }
    let mut store: BackupStore = Resource::new(req.name, spec);
    store.labels = req.labels;
    store.status.store_id = r.store_id;
    // API 노드가 저장소를 쓸 수 있으면 미리 본다(아니면 초기화 Task가 본다).
    let backend = stores::open_backend(&state.cluster, &store, state.node, &state.mount_root).ok();
    let name = store.name.clone();
    let enc = req.encryption.unwrap_or(EncryptionRequest {
        generate: false,
        recovery_key: None,
        escrow_passphrase: None,
    });
    let chosen = usize::from(enc.generate)
        + usize::from(enc.recovery_key.is_some())
        + usize::from(enc.escrow_passphrase.is_some());
    if chosen > 1 {
        return Err(invalid(
            "encryption takes one of generate, recovery_key and escrow_passphrase",
        ));
    }
    if enc.generate && store.spec.read_only {
        return Err(invalid(
            "a read-only store is opened with its key: give recovery_key or escrow_passphrase",
        ));
    }
    let mut recovery_key = None;
    let key = if enc.generate {
        let mk = MasterKey::generate();
        recovery_key = Some(mk.recovery_key());
        Some(mk)
    } else if let Some(k) = &enc.recovery_key {
        Some(MasterKey::from_recovery_key(k).map_err(key_failed)?)
    } else if let Some(pass) = enc.escrow_passphrase {
        let backend = backend.as_ref().ok_or_else(|| {
            ApiError::conflict(
                "store_not_on_node",
                format!(
                    "unlocking the escrowed key needs the store on this node; send the request to a node using storage pool {}",
                    backend_text(&store.spec)
                ),
            )
        })?;
        Some(
            tokio::time::timeout(PROBE_TIMEOUT, unlock_escrow(&name, backend.as_ref(), pass))
                .await
                .map_err(|_| timed_out(&name))??,
        )
    } else {
        None
    };
    if let Some(mk) = &key {
        store.spec.encryption = Some(StoreEncryption {
            fingerprint: StoreKeys::new(mk).fingerprint().to_string(),
            key: mk.to_base64(),
        });
    }
    // 미리 보기: 닿지 않으면 넘어간다(초기화 Task가 같은 것을 본다).
    if let Some(backend) = &backend {
        let probe = async {
            let info = opl_store::store::read_info(backend.as_ref()).await?;
            let owner = match backend.get(opl_store::format::OWNER_FILE).await {
                Ok(b) => serde_json::from_slice::<OwnerFile>(&b).ok(),
                Err(StoreError::NotFound(_)) => None,
                Err(e) => return Err(e),
            };
            Ok((info, owner))
        };
        if let Ok(Ok((info, owner))) = tokio::time::timeout(PROBE_TIMEOUT, probe).await {
            let want = store
                .spec
                .encryption
                .as_ref()
                .map(|e| e.fingerprint.as_str());
            match &info {
                Some(info) if info.fingerprint().is_some() && want.is_none() => {
                    return Err(ApiError::bad_request(format!(
                        "backup store {name:?} is encrypted: give its recovery key or escrow passphrase"
                    ))
                    .with_code("store_key_required"));
                }
                Some(info) if info.fingerprint() != want => {
                    return Err(ApiError::bad_request(format!(
                        "the key does not match backup store {name:?} (fingerprint {}, store has {})",
                        want.unwrap_or("none"),
                        info.fingerprint().unwrap_or("none")
                    ))
                    .with_code("store_key_mismatch"));
                }
                None if store.spec.read_only => {
                    return Err(invalid(format!(
                        "there is no backup store at {} to attach",
                        backend_text(&store.spec)
                    )));
                }
                _ => {}
            }
            if !store.spec.read_only
                && let Some(o) = owner
            {
                let (_, fp) = this_cluster(&state)?;
                if o.ca_fingerprint != fp {
                    return Err(ApiError::conflict(
                        "store_owned_elsewhere",
                        format!(
                            "the store location is owned by cluster {:?}; attach it read-only or take it over",
                            o.cluster
                        ),
                    ));
                }
            }
        }
    }
    if let Some(pass) = req.escrow_passphrase {
        let Some(mk) = &key else {
            return Err(invalid("escrow_passphrase needs an encrypted store"));
        };
        if store.spec.read_only {
            return Err(invalid(
                "a read-only store is not written to; write the escrow from its owner",
            ));
        }
        let backend = backend.as_ref().ok_or_else(|| {
            ApiError::conflict(
                "store_not_on_node",
                "writing the escrowed key needs the store on this node; create the store without it and use key escrow from a node using the store",
            )
        })?;
        write_escrow(&name, backend.as_ref(), mk, pass).await?;
    }
    store.revision = state.cluster.create(&store).await?;
    let node = match task_node(&state, &store) {
        Ok(node) => node,
        Err(e) => {
            if let Err(d) = state
                .cluster
                .delete::<BackupStoreSpec>(store.id, None)
                .await
            {
                tracing::warn!(store = %store.name, error = %d, "cannot remove a store without an init node");
            }
            return Err(e);
        }
    };
    let task = create_task(&state.cluster, TaskOp::BackupStoreInit, store.id, node).await?;
    audit.detail(format!(
        "backup-store create {} ({}{}{})",
        store.name,
        backend_text(&store.spec),
        store
            .spec
            .encryption
            .as_ref()
            .map(|e| format!(", encrypted {}", e.fingerprint))
            .unwrap_or_default(),
        if store.spec.read_only {
            ", read-only"
        } else {
            ""
        }
    ));
    Ok((
        StatusCode::CREATED,
        Json(CreatedStore {
            store: view(&store)?,
            recovery_key,
            task,
        }),
    ))
}

/// 일정·검증·동시 요청 수·S3 자격 증명·CA를 바꾼다. 백엔드·암호화는 바꾸지 못한다(그 칸은 받지 않는다 — 400). 일정이
/// 바뀌면 다음 실행 시각을 다시 고른다.
#[utoipa::path(patch, path = "/backup-stores/{key}", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    request_body = StorePatch,
    responses((status = 200, body = Resource<BackupStoreSpecView, BackupStoreStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
    Json(req): Json<StorePatch>,
) -> Result<Json<BackupStoreView>, ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    for _ in 0..10 {
        let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
        let mut spec = store.spec.clone();
        if let Some(gc) = &req.gc {
            spec.gc = gc.clone();
            spec.gc.days.sort();
        }
        if let Some(verify) = &req.verify {
            spec.verify = verify.clone();
            if let Some(v) = spec.verify.as_mut() {
                v.schedule.days.sort();
            }
        }
        if let Some(n) = req.max_streams {
            spec.max_streams = n;
        }
        match (&mut spec.backend, &req.s3_credentials, &req.ca_pem) {
            (StoreBackend::S3(s3), creds, ca) => {
                if let Some(c) = creds {
                    s3.access_key_id = c.access_key_id.clone();
                    s3.secret_access_key = c.secret_access_key.clone();
                }
                if let Some(ca) = ca {
                    s3.ca_pem = ca.clone();
                }
            }
            (StoreBackend::Pool(_), None, None) => {}
            (StoreBackend::Pool(_), _, _) => {
                return Err(invalid(
                    "s3_credentials and ca_pem are only for S3 backup stores",
                ));
            }
        }
        spec.validate().map_err(invalid)?;
        if spec == store.spec {
            return Ok(Json(view(&store)?));
        }
        let rescheduled = spec.gc != store.spec.gc || spec.verify != store.spec.verify;
        match state
            .cluster
            .update_spec(store.id, Some(store.revision), store.labels.clone(), &spec)
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
        // 다음 실행 시각은 스케줄러가 새 일정으로 다시 적는다.
        if rescheduled {
            reset_schedule(&state, store.id).await?;
        }
        return Ok(Json(view(&lookup::<BackupStoreSpec>(
            &state.cluster,
            &store.id.to_string(),
        )?)?));
    }
    Err(ApiError::conflict(
        "busy",
        "the backup store kept changing while being updated; try again",
    ))
}

/// 다음 GC·검증 시각을 지운다(리더 스케줄러가 새 일정으로 적는다).
async fn reset_schedule(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    for _ in 0..10 {
        let Some(store) = state.cluster.get::<BackupStoreSpec>(id)? else {
            return Ok(());
        };
        let mut status = store.status.clone();
        status.next_gc = None;
        status.next_verify = None;
        match state
            .cluster
            .update_status_checked::<BackupStoreSpec>(id, store.revision, &status)
            .await
        {
            Ok(_) => return Ok(()),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// 저장소 리소스를 지운다(200). 저장소 안 데이터는 그대로다(버킷·디렉터리는 손으로). 저장소의 백업이 남아 있으면 409
/// `store_in_use`, 저장소 Task나 저장소 백업을 쓰는 Task가 끝나지 않았으면 409 `store_busy`.
#[utoipa::path(delete, path = "/backup-stores/{key}", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<BackupStoreSpecView, BackupStoreStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<BackupStoreView>, ApiError> {
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    let (_, backups) = state.cluster.list::<BackupSpec>()?;
    let n = backups
        .iter()
        .filter(|b| b.spec.store.as_deref() == Some(store.name.as_str()))
        .count();
    if n > 0 {
        return Err(ApiError::conflict(
            "store_in_use",
            format!(
                "backup store {:?} holds {n} backup(s); delete them first",
                store.name
            ),
        ));
    }
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let busy = tasks.iter().any(|t| {
        !t.status.state.is_finished()
            && (t.spec.target == store.id
                || matches!(&t.spec.op, TaskOp::BackupConvert { store: s } | TaskOp::BackupCopy { store: s } if *s == store.name))
    });
    if busy {
        return Err(ApiError::conflict(
            "store_busy",
            format!(
                "a task is using backup store {:?}; try again when it has finished",
                store.name
            ),
        ));
    }
    state
        .cluster
        .delete::<BackupStoreSpec>(store.id, Some(store.revision))
        .await?;
    Ok(Json(view(&store)?))
}

/// 저장소 동작 본문(검증만 쓴다)
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StoreVerifyRequest {
    /// 검증할 백업(이름이나 id). 비면 일정의 규칙대로 고른다(검증한 적 없거나 오래된 백업).
    #[serde(default)]
    pub backups: Vec<String>,
}

/// GC를 지금 돌린다(202 Task). 준비되지 않았으면 409 `store_not_ready`, 읽기 전용이면 409 `store_read_only`, 이미 돌면
/// 409 `store_busy`.
#[utoipa::path(post, path = "/backup-stores/{key}/actions/gc", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, opl_types::task::TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn gc_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    crate::backups::check_store(&store, true)?;
    check_not_running(&state, &store, &TaskOp::BackupStoreGc)?;
    let node = task_node(&state, &store)?;
    let task = create_task(&state.cluster, TaskOp::BackupStoreGc, store.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 검증을 지금 돌린다(202 Task). `backups`가 비면 일정의 규칙대로 고른다. 읽기 전용 저장소도 검증한다(깨진 청크를
/// 옮기지 않고 결과만 적는다).
#[utoipa::path(post, path = "/backup-stores/{key}/actions/verify", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    request_body = StoreVerifyRequest,
    responses((status = 202, body = Resource<TaskSpec, opl_types::task::TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn verify_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
    body: Option<Json<StoreVerifyRequest>>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    ready(&store)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let mut backups = Vec::with_capacity(req.backups.len());
    for key in &req.backups {
        let b = lookup::<BackupSpec>(&state.cluster, key)?;
        if b.spec.store.as_deref() != Some(store.name.as_str()) {
            return Err(ApiError::bad_request(format!(
                "backup {:?} is not in backup store {:?}",
                b.name, store.name
            )));
        }
        backups.push(b.id);
    }
    let task = start_verify(&state, &store, backups).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 저장소 검증 Task를 만든다(백업 검증 API도 쓴다).
pub(crate) async fn start_verify(
    state: &AppState,
    store: &BackupStore,
    backups: Vec<Uuid>,
) -> Result<Task, ApiError> {
    let node = task_node(state, store)?;
    create_task(
        &state.cluster,
        TaskOp::BackupStoreVerify { backups },
        store.id,
        node,
    )
    .await
}

/// 연결 확인(200): API 노드가 그 저장소를 쓸 수 있으면 `store.json`을 읽어 키를 맞춰 보고, 노드마다 보고한 닿음을 함께
/// 돌려준다.
#[utoipa::path(post, path = "/backup-stores/{key}/actions/check", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = StoreCheck), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<StoreCheck>, ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    let (ok, checked_here, error) = match backend_here(&state, &store) {
        Err(_) => (false, false, None),
        Ok(_) => match open_here(&state, &store).await {
            Ok(_) => (true, true, None),
            Err(e) => (false, true, Some(e.message)),
        },
    };
    let (_, nodes) = state.cluster.list::<opl_types::node::NodeSpec>()?;
    let mut reports: Vec<StoreNodeCheck> = nodes
        .iter()
        .filter_map(|n| {
            n.status
                .backup_stores
                .iter()
                .find(|s| s.store == store.id)
                .map(|s| StoreNodeCheck {
                    node: n.id,
                    name: n.name.clone(),
                    ok: s.ok,
                    error: s.error.clone(),
                    checked_at: s.checked_at,
                })
        })
        .collect();
    reports.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(StoreCheck {
        ok,
        checked_here,
        error,
        nodes: reports,
    }))
}

/// 읽기 전용 저장소의 스냅샷마다 백업 리소스를 만든다(202 Task — §12.6). 다시 하면 새것만 더한다. 읽기 전용이 아니면
/// 409 `store_read_only`의 반대 — 400.
#[utoipa::path(post, path = "/backup-stores/{key}/actions/scan", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, opl_types::task::TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn scan_store(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    ready(&store)?;
    if !store.spec.read_only {
        return Err(ApiError::bad_request(format!(
            "backup store {:?} is this cluster's own store; scan imports backups of a store attached read-only",
            store.name
        )));
    }
    check_not_running(&state, &store, &TaskOp::BackupStoreScan)?;
    let node = task_node(&state, &store)?;
    let task = create_task(&state.cluster, TaskOp::BackupStoreScan, store.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 주 키를 복구 키로 돌려준다(200, 감사에 남음). 평문 저장소면 400.
#[utoipa::path(post, path = "/backup-stores/{key}/key/export", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RecoveryKey), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn export_key(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<RecoveryKey>, ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    let mk = stores::master_key(&store)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::bad_request(format!("backup store {:?} is not encrypted", store.name))
        })?;
    audit.detail(format!("backup-store key export {}", store.name));
    Ok(Json(RecoveryKey {
        recovery_key: mk.recovery_key(),
    }))
}

/// 주 키를 암호 문구로 감싸 저장소 안(`keys/<지문>.json`)에 쓴다(204 — 덮어씀). 암호 문구는 저장하지 않는다. 평문
/// 저장소면 400, 읽기 전용이면 409 `store_read_only`, API 노드가 그 저장소를 쓸 수 없으면 409 `store_not_on_node`,
/// 닿지 않으면 503 `store_unavailable`.
#[utoipa::path(post, path = "/backup-stores/{key}/key/escrow", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    request_body = EscrowRequest,
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody), (status = 503, body = crate::error::ErrorBody)))]
pub async fn escrow_key_handler(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<EscrowRequest>,
) -> Result<StatusCode, ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    let mk = stores::master_key(&store)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::bad_request(format!("backup store {:?} is not encrypted", store.name))
        })?;
    crate::backups::check_store(&store, true)?;
    // 키가 그 저장소의 것인지 연 뒤에 쓴다.
    open_here(&state, &store).await?;
    let backend = backend_here(&state, &store)?;
    write_escrow(&store.name, backend.as_ref(), &mk, req.passphrase).await?;
    audit.detail(format!("backup-store key escrow {}", store.name));
    Ok(StatusCode::NO_CONTENT)
}

/// 읽기 전용으로 붙인 저장소를 넘겨받는다(200 — §12.6): 주인 표식(`owner.json`)의 하트비트가 24시간보다 오래됐거나
/// `force`면 주인을 이 클러스터로 바꾸고 `read_only`를 끈다. 옛 주인은 다음 GC 때 바뀐 표식을 보고 그 저장소를 멈춘다.
/// 하트비트가 새로우면 409 `store_owned_elsewhere`, 읽기 전용이 아니면 400.
#[utoipa::path(post, path = "/backup-stores/{key}/actions/take-over", tag = "backup-stores",
    params(("key" = String, Path, description = "id or name")),
    request_body = TakeOverRequest,
    responses((status = 200, body = Resource<BackupStoreSpecView, BackupStoreStatus>), (status = "4XX", body = crate::error::ErrorBody), (status = 503, body = crate::error::ErrorBody)))]
pub async fn take_over(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    body: Option<Json<TakeOverRequest>>,
) -> Result<Json<BackupStoreView>, ApiError> {
    require_feature(&state.cluster, BACKUP_STORE)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let store = lookup::<BackupStoreSpec>(&state.cluster, &key)?;
    ready(&store)?;
    if !store.spec.read_only {
        return Err(ApiError::bad_request(format!(
            "backup store {:?} is already this cluster's store",
            store.name
        )));
    }
    let opened = open_here(&state, &store).await?;
    let (cluster, fp) = this_cluster(&state)?;
    let now = now();
    let owner = tokio::time::timeout(PROBE_TIMEOUT, opened.read_owner())
        .await
        .map_err(|_| timed_out(&store.name))?
        .map_err(|e| store_failed(&store.name, e))?;
    let from = owner.as_ref().map(|o| o.cluster.clone());
    if let Some(o) = &owner
        && o.ca_fingerprint != fp
        && o.heartbeat.saturating_add(TAKE_OVER_AFTER) > now
        && !req.force
    {
        return Err(ApiError::conflict(
            "store_owned_elsewhere",
            format!(
                "cluster {:?} wrote its heartbeat to the store {} minute(s) ago; take over only when it is gone (or with force)",
                o.cluster,
                now.saturating_sub(o.heartbeat) / 60
            ),
        ));
    }
    let me = OwnerFile {
        cluster: cluster.clone(),
        ca_fingerprint: fp.clone(),
        store_resource: store.id,
        heartbeat: now,
    };
    tokio::time::timeout(PROBE_TIMEOUT, opened.write_owner(&me))
        .await
        .map_err(|_| timed_out(&store.name))?
        .map_err(|e| store_failed(&store.name, e))?;
    let mut spec = store.spec.clone();
    spec.read_only = false;
    state
        .cluster
        .update_spec(store.id, Some(store.revision), store.labels.clone(), &spec)
        .await?;
    // 주인·다음 GC(쓰기 저장소가 됐다)
    for _ in 0..10 {
        let Some(cur) = state.cluster.get::<BackupStoreSpec>(store.id)? else {
            break;
        };
        let mut status = cur.status.clone();
        status.owner = Some(StoreOwner {
            cluster: cluster.clone(),
            ca_fingerprint: fp.clone(),
        });
        status.next_gc = Some(next_run(&cur.spec.gc, now));
        status.error = None;
        match state
            .cluster
            .update_status_checked::<BackupStoreSpec>(store.id, cur.revision, &status)
            .await
        {
            Ok(_) => break,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
    audit.detail(format!(
        "backup-store take-over {}{}",
        store.name,
        from.map(|c| format!(" (from {c})")).unwrap_or_default()
    ));
    let store = lookup::<BackupStoreSpec>(&state.cluster, &store.id.to_string())?;
    if store.status.phase != StorePhase::Ready {
        tracing::warn!(store = %store.name, "taken-over store is not ready");
    }
    Ok(Json(view(&store)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_and_watch_events_drop_the_secrets() {
        let spec: BackupStoreSpec = serde_json::from_value(serde_json::json!({
            "backend": {"s3": {"endpoint": "https://s3.example.com", "bucket": "bk",
                               "access_key_id": "AKID", "secret_access_key": "SECRET"}},
            "encryption": {"fingerprint": "00:11:22:33:44:55:66:77:88:99:aa:bb:cc:dd:ee:ff",
                           "key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}
        }))
        .unwrap();
        let store: BackupStore = Resource::new("st", spec);
        let v = serde_json::to_string(&view(&store).unwrap()).unwrap();
        assert!(!v.contains("SECRET") && !v.contains("AAAAAAAA"), "{v}");
        assert!(v.contains("AKID") && v.contains("00:11:22"), "{v}");
        // watch 이벤트(같은 함수)와 보기는 같은 모양이다.
        let mut raw = serde_json::to_value(&store).unwrap();
        redact_json(&mut raw["spec"]);
        assert_eq!(raw, serde_json::to_value(view(&store).unwrap()).unwrap());
    }
}
