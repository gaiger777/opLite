//! 저장한 VMware 원본 연결 `/api/v1/vmware-sources`(계획 72 설계 §8.1·§8.2): 만들기(지문 확인 흐름 — 계획 30과 같다)·
//! 바꾸기·지우기·다시 확인, 인벤토리 찾아보기. 연결은 루트 종류다 — 읽기는 `cluster.read`, 바꾸기는 `cluster.admin`(vCenter
//! 비밀번호를 든다), 찾아보기는 루트 `vm.create`(경로표). 비밀번호는 Raft 스펙에만 있고 보기·watch·감사에는 없다
//! ([`VmwareSourceView`]).
//!
//! 노드마다 원본별 세션 캐시(5분 쉬면 로그아웃)와 인벤토리 캐시(60초)를 둔다([`VsphereCache`]) — 찾아보기·미리 확인이
//! 요청마다 로그인하거나 vCenter 인벤토리를 다시 읽지 않게. 캐시는 연결 스펙이 바뀌면(비밀번호·지문 등) 쓰지 않는다.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_types::node::VMWARE_MIGRATE;
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::vm_import::normalize_fingerprint;
use opl_types::vmware::{
    Secret, VddkTransport, VmwareMigrationSpec, VmwareSource, VmwareSourceSpec, VmwareSourceStatus,
    VmwareTransport, VmwareVmSpec, redact_source_json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;
use crate::vmware::client::Session;
use crate::vmware::inventory::{self, Inventory};
use crate::vmware::{VsphereError, http};

/// 쓰지 않은 세션을 닫는 시간(설계 §7.1)
pub const SESSION_IDLE: Duration = Duration::from_secs(300);
/// 인벤토리를 다시 읽지 않고 돌려주는 시간(설계 §8.2)
pub const INVENTORY_TTL: Duration = Duration::from_secs(60);
/// 캐시를 훑어 오래된 세션을 닫는 간격
const SWEEP_EVERY: Duration = Duration::from_secs(30);

// ---- 캐시 ----

struct CachedSession {
    spec: VmwareSourceSpec,
    session: Arc<Session>,
    used: Instant,
}

struct CachedInventory {
    spec: VmwareSourceSpec,
    inventory: Arc<Inventory>,
    fetched: Instant,
    fetched_at: u64,
}

#[derive(Default)]
struct CacheInner {
    sessions: Mutex<HashMap<Uuid, CachedSession>>,
    inventories: Mutex<HashMap<Uuid, CachedInventory>>,
    sweeping: AtomicBool,
}

/// 노드의 원본별 vSphere 세션·인벤토리 캐시. 세션은 [`SESSION_IDLE`] 쉬면 버리고(버리면 `Session`이 로그아웃을 띄운다),
/// 인벤토리는 [`INVENTORY_TTL`] 동안 그대로 준다. 열쇠는 원본 id이고 저장한 스펙이 지금 스펙과 다르면 쓰지 않는다.
pub struct VsphereCache {
    inner: Arc<CacheInner>,
    session_idle: Duration,
    inventory_ttl: Duration,
}

impl Default for VsphereCache {
    fn default() -> Self {
        Self::new(SESSION_IDLE, INVENTORY_TTL)
    }
}

impl std::fmt::Debug for VsphereCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VsphereCache")
            .field("session_idle", &self.session_idle)
            .field("inventory_ttl", &self.inventory_ttl)
            .finish_non_exhaustive()
    }
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl VsphereCache {
    /// 시험이 시간을 줄인다.
    pub fn new(session_idle: Duration, inventory_ttl: Duration) -> Self {
        Self {
            inner: Arc::default(),
            session_idle,
            inventory_ttl,
        }
    }

    /// 원본의 세션(캐시에 없거나 스펙이 바뀌었으면 새로 로그인한다)
    pub async fn session(&self, source: &VmwareSource) -> Result<Arc<Session>, VsphereError> {
        if let Some(c) = locked(&self.inner.sessions).get_mut(&source.id)
            && c.spec == source.spec
            && c.used.elapsed() < self.session_idle
        {
            c.used = Instant::now();
            return Ok(c.session.clone());
        }
        let session = Arc::new(Session::login(&source.spec).await?);
        self.keep(source, session.clone());
        Ok(session)
    }

    /// 로그인한 세션을 캐시에 둔다(옛 것은 버려 로그아웃).
    pub fn keep(&self, source: &VmwareSource, session: Arc<Session>) {
        let old = locked(&self.inner.sessions).insert(
            source.id,
            CachedSession {
                spec: source.spec.clone(),
                session,
                used: Instant::now(),
            },
        );
        drop(old);
        self.start_sweeper();
    }

    /// 원본의 인벤토리와 읽은 시각(유닉스 초). `refresh`면 캐시를 쓰지 않는다. 읽기가 실패하면 그 세션을 버린다(다음
    /// 요청이 다시 로그인한다).
    pub async fn inventory(
        &self,
        source: &VmwareSource,
        refresh: bool,
    ) -> Result<(Arc<Inventory>, u64), VsphereError> {
        if !refresh
            && let Some(c) = locked(&self.inner.inventories).get(&source.id)
            && c.spec == source.spec
            && c.fetched.elapsed() < self.inventory_ttl
        {
            return Ok((c.inventory.clone(), c.fetched_at));
        }
        let session = self.session(source).await?;
        let read = match inventory::inventory(&session).await {
            Ok(inv) => Arc::new(inv),
            Err(e) => {
                self.forget_session(source.id);
                return Err(e);
            }
        };
        let fetched_at = now();
        locked(&self.inner.inventories).insert(
            source.id,
            CachedInventory {
                spec: source.spec.clone(),
                inventory: read.clone(),
                fetched: Instant::now(),
                fetched_at,
            },
        );
        Ok((read, fetched_at))
    }

    fn forget_session(&self, id: Uuid) {
        let old = locked(&self.inner.sessions).remove(&id);
        drop(old);
    }

    /// 원본의 세션·인벤토리를 버린다(바꾸기·지우기·다시 확인).
    pub fn forget(&self, id: Uuid) {
        self.forget_session(id);
        locked(&self.inner.inventories).remove(&id);
    }

    /// 쉰 세션과 지난 인벤토리를 버린다.
    fn sweep(inner: &CacheInner, session_idle: Duration, inventory_ttl: Duration) {
        let idle: Vec<CachedSession> = {
            let mut sessions = locked(&inner.sessions);
            let ids: Vec<Uuid> = sessions
                .iter()
                .filter(|(_, c)| c.used.elapsed() >= session_idle)
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| sessions.remove(id)).collect()
        };
        // 자물쇠 밖에서 버린다(Drop이 로그아웃을 띄운다).
        drop(idle);
        locked(&inner.inventories).retain(|_, c| c.fetched.elapsed() < inventory_ttl);
    }

    /// 캐시를 훑는 작업을 한 번만 띄운다(캐시가 없어지면 끝난다).
    fn start_sweeper(&self) {
        if self.inner.sweeping.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.inner.sweeping.store(false, Ordering::SeqCst);
            return;
        };
        let weak: Weak<CacheInner> = Arc::downgrade(&self.inner);
        let (idle, ttl) = (self.session_idle, self.inventory_ttl);
        let every = SWEEP_EVERY.min(idle).max(Duration::from_millis(10));
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                Self::sweep(&inner, idle, ttl);
            }
        });
    }
}

// ---- 보기 ----

/// 비밀번호를 뺀 원본 스펙
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VmwareSourceSpecView {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// 믿기로 한 서버 인증서의 SHA-256 지문
    pub fingerprint: String,
    #[serde(default, skip_serializing_if = "VmwareTransport::is_folder")]
    pub transport: VmwareTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub datacenter_hint: Option<String>,
}

/// 응답의 원본 연결
pub type VmwareSourceView = Resource<VmwareSourceSpecView, VmwareSourceStatus>;

/// 비밀번호를 뺀 보기
pub(crate) fn view(source: &VmwareSource) -> Result<VmwareSourceView, ApiError> {
    let mut v = serde_json::to_value(source).map_err(ApiError::internal)?;
    redact_source_json(&mut v["spec"]);
    serde_json::from_value(v).map_err(ApiError::internal)
}

// ---- 요청 ----

/// VDDK 길(운영자가 설치한 nbdkit `vddk` 플러그인 — vSAN·vVols). 썸프린트는 서버가 잰다.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VddkRequest {
    /// VDDK 라이브러리 디렉터리. 없으면 노드 설정 `[vmware] vddk_libdir`다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub libdir: Option<String>,
}

/// 원본 연결 만들기 본문
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSourceRequest {
    pub name: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// vCenter·ESXi 호스트 이름 또는 IP
    pub host: String,
    /// 기본 443
    #[serde(default)]
    #[schema(nullable = false)]
    pub port: Option<u16>,
    pub user: String,
    /// 저장한다(Raft). 응답·watch·감사에는 없다.
    #[schema(write_only)]
    pub password: String,
    /// 믿을 서버 인증서의 SHA-256 지문. 없으면 409 `untrusted_certificate`(`details.fingerprint`)로 알려 준다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub fingerprint: Option<String>,
    /// 디스크를 VDDK로 읽는다(없으면 데이터스토어 `/folder` 범위 읽기).
    #[serde(default)]
    #[schema(nullable = false)]
    pub vddk: Option<VddkRequest>,
    /// 데이터센터 이름 힌트(ESXi는 `ha-datacenter`)
    #[serde(default)]
    #[schema(nullable = false)]
    pub datacenter_hint: Option<String>,
}

impl std::fmt::Debug for CreateSourceRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSourceRequest")
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"***")
            .field("fingerprint", &self.fingerprint)
            .field("vddk", &self.vddk)
            .finish_non_exhaustive()
    }
}

/// 원본 연결에서 바꿀 값(주어진 것만). 바꾸면 다시 로그인해 본 뒤 저장한다 — 쓰는 계획은 다음 Task부터 새 값을 쓴다.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SourcePatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub host: Option<String>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub port: Option<u16>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub user: Option<String>,
    #[serde(default)]
    #[schema(nullable = false, write_only)]
    pub password: Option<String>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub fingerprint: Option<String>,
    /// VDDK 길. `null`이면 `/folder` 범위 읽기로 돌아간다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<VddkRequest>, nullable)]
    pub vddk: Option<Option<VddkRequest>>,
    /// `null`이면 지운다.
    #[serde(default, deserialize_with = "crate::backup_jobs::present")]
    #[schema(value_type = Option<String>, nullable)]
    pub datacenter_hint: Option<Option<String>>,
}

impl std::fmt::Debug for SourcePatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcePatch")
            .field("host", &self.host)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct InventoryQuery {
    /// 노드 캐시(60초)를 쓰지 않고 다시 읽는다.
    #[serde(default)]
    pub refresh: bool,
}

/// 끝나지 않은 이전 계획에 든 원본 VM(같은 VM은 계획 하나에만)
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MigratingVm {
    /// 원본 VM의 `config.instanceUuid`
    pub instance_uuid: String,
    /// 이전 계획 이름
    pub migration: String,
    /// 항목(`vmware-vm`) 이름
    pub item: String,
}

/// 인벤토리 찾아보기의 답
#[derive(Debug, Serialize, ToSchema)]
pub struct SourceInventory {
    /// 원본에서 읽은 시각(유닉스 초 — 노드 캐시면 그때)
    pub fetched_at: u64,
    pub inventory: Inventory,
    /// 이미 끝나지 않은 계획에 든 VM(항목을 볼 수 있을 때만)
    pub migrating: Vec<MigratingVm>,
}

// ---- 도움 ----

fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("invalid_vmware_source")
}

/// 지문 확인 흐름: 서버 인증서의 지문 둘을 잰다(자격 증명은 보내지 않는다).
async fn probe(host: &str, port: u16) -> Result<http::CertInfo, ApiError> {
    http::probe(host, port)
        .await
        .map_err(|e| VsphereError::from(e).api_error())
}

/// VDDK 길이면 서버 인증서의 SHA-1 썸프린트를 잰다(지금 인증서가 믿기로 한 지문과 같아야 한다 — 아니면 409
/// `fingerprint_mismatch`).
async fn vddk_transport(
    spec: &VmwareSourceSpec,
    vddk: Option<VddkRequest>,
) -> Result<VmwareTransport, ApiError> {
    let Some(vddk) = vddk else {
        return Ok(VmwareTransport::Folder);
    };
    let cert = probe(&spec.host, spec.port).await?;
    if normalize_fingerprint(&cert.sha256).as_deref() != Some(spec.fingerprint.as_str()) {
        return Err(VsphereError::Fingerprint {
            expected: spec.fingerprint.clone(),
            actual: cert.sha256,
        }
        .api_error());
    }
    Ok(VmwareTransport::Vddk(VddkTransport {
        libdir: vddk.libdir,
        thumbprint_sha1: cert.sha1,
    }))
}

/// 원본을 쓰는 이전 계획이 있으면 409 `source_in_use`
fn check_unused(state: &AppState, source: &VmwareSource) -> Result<(), ApiError> {
    let (_, migrations) = state.cluster.list::<VmwareMigrationSpec>()?;
    match migrations.iter().find(|m| m.spec.source == source.id) {
        Some(m) => Err(ApiError::conflict(
            "source_in_use",
            format!(
                "vmware-source {:?} is used by vmware-migration {:?}; delete the migration first",
                source.name, m.name
            ),
        )),
        None => Ok(()),
    }
}

/// 로그인해 원본 상태를 잰다.
async fn login_status(
    spec: &VmwareSourceSpec,
) -> Result<(Session, VmwareSourceStatus), VsphereError> {
    let session = Session::login(spec).await?;
    let status = session.source_status(now());
    Ok((session, status))
}

// ---- 핸들러 ----

#[utoipa::path(get, path = "/vmware-sources", tag = "vmware",
    responses((status = 200, body = List<Resource<VmwareSourceSpecView, VmwareSourceStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_sources(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<VmwareSourceView>>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::VmwareSource)?;
    let (revision, sources) = state.cluster.list::<VmwareSourceSpec>()?;
    let items = sources.iter().map(view).collect::<Result<_, _>>()?;
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vmware-sources/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<VmwareSourceSpecView, VmwareSourceStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_source(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<VmwareSourceView>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::VmwareSource)?;
    Ok(Json(view(&lookup::<VmwareSourceSpec>(
        &state.cluster,
        &key,
    )?)?))
}

/// 원본 연결을 만든다(201): 로그인해 종류(vCenter·ESXi)·판을 `status`에 적는다. 지문이 없으면 연결만 해 409
/// `untrusted_certificate`(`details.fingerprint`·`details.thumbprint_sha1` — 확인하고 다시 보낸다), 다르면 409
/// `fingerprint_mismatch`. 로그인이 거절되면 400 `vmware_auth`, 닿지 않으면 502 `vmware_unreachable`. 비밀번호는 응답·
/// watch·감사에 없다. 모든 노드가 `vmware-migrate`를 알아야 한다(409 `cluster_upgrading`).
#[utoipa::path(post, path = "/vmware-sources", tag = "vmware",
    request_body = CreateSourceRequest,
    responses((status = 201, body = Resource<VmwareSourceSpecView, VmwareSourceStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_source(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateSourceRequest>,
) -> Result<(StatusCode, Json<VmwareSourceView>), ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    check_name(&req.name)?;
    let mut spec = VmwareSourceSpec {
        host: req.host,
        port: req.port.unwrap_or(443),
        user: req.user,
        password: Secret::new(req.password),
        // 지문이 없으면 모양만 맞는 값으로 다른 칸을 먼저 본다(그 뒤 서버 지문을 알려 준다).
        fingerprint: req.fingerprint.clone().unwrap_or_else(|| "00".repeat(32)),
        transport: VmwareTransport::Folder,
        datacenter_hint: req.datacenter_hint,
    };
    spec.normalize().map_err(invalid)?;
    if req.fingerprint.is_none() {
        let cert = probe(&spec.host, spec.port).await?;
        return Err(VsphereError::Untrusted {
            sha256: cert.sha256,
            sha1: cert.sha1,
        }
        .api_error());
    }
    spec.transport = vddk_transport(&spec, req.vddk).await?;
    spec.normalize().map_err(invalid)?;
    let mut source: VmwareSource = Resource::new(req.name, spec);
    source.labels = req.labels;
    audit.detail(format!(
        "vmware-source create {} ({})",
        source.name,
        source.spec.audit_summary()
    ));
    let (session, status) = login_status(&source.spec)
        .await
        .map_err(VsphereError::api_error)?;
    source.status = status;
    state.cluster.create(&source).await?;
    state.vmware_cache.keep(&source, Arc::new(session));
    Ok((StatusCode::CREATED, Json(view(&source)?)))
}

/// 원본 연결을 바꾼다: 주어진 값만 바꾸고 다시 로그인해 본 뒤 저장한다(로그인이 거절되면 저장하지 않는다). 호스트·포트·
/// 사용자를 바꾸면 비밀번호도 다시 줘야 한다(아니면 400 `password_required`). 지문을 바꾸려면 새 지문을 준다(서버 인증서가
/// 바뀐 뒤). VDDK 길이면 썸프린트를 다시 잰다. `If-Match`로 revision을 볼 수 있다.
#[utoipa::path(patch, path = "/vmware-sources/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = SourcePatch,
    responses((status = 200, body = Resource<VmwareSourceSpecView, VmwareSourceStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn patch_source(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<SourcePatch>,
) -> Result<Json<VmwareSourceView>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let source = lookup::<VmwareSourceSpec>(&state.cluster, &key)?;
    let expected = if_match(&headers)?.unwrap_or(source.revision);
    let mut spec = source.spec.clone();
    let endpoint_changed =
        patch.host.is_some() || patch.port.is_some() || patch.fingerprint.is_some();
    if let Some(h) = patch.host {
        spec.host = h;
    }
    if let Some(p) = patch.port {
        spec.port = p;
    }
    if let Some(u) = patch.user {
        spec.user = u;
    }
    let password_given = patch.password.is_some();
    if let Some(p) = patch.password {
        spec.password = Secret::new(p);
    }
    if let Some(f) = patch.fingerprint {
        spec.fingerprint = f;
    }
    if let Some(d) = patch.datacenter_hint {
        spec.datacenter_hint = d;
    }
    // 전송 길: 바꾸라 했거나 주소·지문이 바뀌면 VDDK 썸프린트를 다시 잰다. 아니면 그대로다.
    let keep = patch.vddk.is_none() && !endpoint_changed;
    let want = match patch.vddk {
        Some(v) => v,
        None => match &spec.transport {
            VmwareTransport::Vddk(v) => Some(VddkRequest {
                libdir: v.libdir.clone(),
            }),
            VmwareTransport::Folder => None,
        },
    };
    if !keep {
        spec.transport = VmwareTransport::Folder;
    }
    spec.normalize().map_err(invalid)?;
    // 저장한 비밀번호는 같은 호스트·포트·사용자에게만 보낸다(다른 서버로 돌려 비밀번호를 빼 가지 못하게).
    let moved = spec.host != source.spec.host
        || spec.port != source.spec.port
        || spec.user != source.spec.user;
    if moved && !password_given {
        return Err(ApiError::bad_request(
            "give the password again when changing the host, port or user",
        )
        .with_code("password_required"));
    }
    if !keep {
        spec.transport = vddk_transport(&spec, want).await?;
        spec.normalize().map_err(invalid)?;
    }
    audit.detail(format!(
        "vmware-source update {} ({})",
        source.name,
        spec.audit_summary()
    ));
    let (session, status) = login_status(&spec).await.map_err(VsphereError::api_error)?;
    state
        .cluster
        .update_spec::<VmwareSourceSpec>(source.id, Some(expected), source.labels.clone(), &spec)
        .await?;
    state
        .cluster
        .update_status::<VmwareSourceSpec>(source.id, &status)
        .await?;
    state.vmware_cache.forget(source.id);
    let updated = lookup::<VmwareSourceSpec>(&state.cluster, &source.id.to_string())?;
    state.vmware_cache.keep(&updated, Arc::new(session));
    Ok(Json(view(&updated)?))
}

/// 원본 연결을 지운다(204). 쓰는 이전 계획이 있으면 409 `source_in_use`.
#[utoipa::path(delete, path = "/vmware-sources/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_source(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let source = lookup::<VmwareSourceSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-source delete {}", source.name));
    check_unused(&state, &source)?;
    state
        .cluster
        .delete::<VmwareSourceSpec>(source.id, if_match(&headers)?)
        .await?;
    state.vmware_cache.forget(source.id);
    Ok(StatusCode::NO_CONTENT)
}

/// 원본에 다시 로그인해 종류·판을 `status`에 적는다(캐시를 쓰지 않는다). 실패하면 `status.error`에 적고 그 오류를
/// 돌려준다(400 `vmware_auth`, 502 `vmware_unreachable`, 409 `fingerprint_mismatch` …).
#[utoipa::path(post, path = "/vmware-sources/{key}/actions/check", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<VmwareSourceSpecView, VmwareSourceStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn check_source(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<VmwareSourceView>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let mut source = lookup::<VmwareSourceSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-source check {}", source.name));
    state.vmware_cache.forget(source.id);
    let result = login_status(&source.spec).await;
    let (status, outcome) = match result {
        Ok((session, status)) => (status, Ok(session)),
        Err(e) => (
            VmwareSourceStatus {
                checked_at: Some(now()),
                error: Some(e.to_string()),
                ..source.status.clone()
            },
            Err(e),
        ),
    };
    state
        .cluster
        .update_status::<VmwareSourceSpec>(source.id, &status)
        .await?;
    source.status = status;
    let session = outcome.map_err(VsphereError::api_error)?;
    state.vmware_cache.keep(&source, Arc::new(session));
    Ok(Json(view(&source)?))
}

/// 끝나지 않은 항목의 원본 VM(찾아보기 표의 "이미 계획에 든 VM")
pub(crate) fn migrating(state: &AppState) -> Result<Vec<MigratingVm>, ApiError> {
    let (_, items) = state.cluster.list::<VmwareVmSpec>()?;
    let (_, migrations) = state.cluster.list::<VmwareMigrationSpec>()?;
    Ok(items
        .iter()
        .filter(|i| !i.status.state.is_final())
        .map(|i| MigratingVm {
            instance_uuid: i.spec.instance_uuid.clone(),
            migration: migrations
                .iter()
                .find(|m| m.id == i.spec.migration)
                .map_or_else(|| i.spec.migration.to_string(), |m| m.name.clone()),
            item: i.name.clone(),
        })
        .collect())
}

/// 원본의 인벤토리(데이터센터·폴더·클러스터·호스트·데이터스토어·네트워크·VM — §7.3). 노드가 60초 담아 두고
/// (`refresh=true`면 다시 읽는다) 비밀은 없다. 이전 계획 항목을 볼 수 있으면 끝나지 않은 계획에 든 VM도 알린다. 닿지
/// 않으면 502 `vmware_unreachable`, 로그인이 거절되면 400 `vmware_auth`.
#[utoipa::path(get, path = "/vmware-sources/{key}/inventory", tag = "vmware",
    params(("key" = String, Path, description = "id or name"), InventoryQuery),
    responses((status = 200, body = SourceInventory), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn source_inventory(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<InventoryQuery>,
) -> Result<Json<SourceInventory>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let source = lookup::<VmwareSourceSpec>(&state.cluster, &key)?;
    let (inventory, fetched_at) = state
        .vmware_cache
        .inventory(&source, q.refresh)
        .await
        .map_err(VsphereError::api_error)?;
    let migrating = if state
        .authorize(&principal, Action::Read, VmwareVmSpec::KIND)
        .is_ok()
    {
        migrating(&state)?
    } else {
        Vec::new()
    };
    Ok(Json(SourceInventory {
        fetched_at,
        inventory: (*inventory).clone(),
        migrating,
    }))
}
