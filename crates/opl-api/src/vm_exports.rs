//! VM 내보내기 API(계획 72 §12): 꺼진 VM을 파일 풀(dir·NFS)의 `orbivisor-exports/{id}/`에 OVF 패키지(서술자 + streamOptimized
//! VMDK + SHA-256 매니페스트)로 쓰고(Task `vm-export`), 그 파일들을 하나의 OVA로 흘려 내려받게 한다.
//!
//! - **권한**: 만들기는 그 VM의 `vm.export`(경로표)와 풀의 `storage.allocate`(핸들러), 보기·지우기·링크·내려받기는 내보내기의
//!   주인인 VM(지운 VM이면 루트)의 `system.read`·`vm.export`다.
//! - **내려받기**는 두 걸음이다: `download-link`가 10분 사는 링크를 서명하고(클러스터 CA 키에서 HKDF로 뽑은 HMAC 키 — 어느 노드든
//!   확인한다, 요청한 사용자에 묶임), `GET …/download/{token}`이 같은 사용자의 인증과 권한을 다시 보고 흘려 보낸다. 파일이 이
//!   노드에서 보이지 않으면(다른 노드의 로컬 dir 풀) 파일을 쓴 노드에서 노드 사이 흐름 `ExportDownload`로 받아 넘긴다. 감사는
//!   흐름이 끝날 때 바이트와 함께.

pub mod tar;

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use opl_cluster::Cluster;
use opl_cluster::types::NodeId;
use opl_peer::{
    DownloadHead, DownloadPiece, DownloadStream, ExportDownloadRequest, ExportSource,
    file_restore::FileRestoreFault,
};
use opl_types::backup::default_backup_name;
use opl_types::file_restore::{DownloadLink, content_disposition};
use opl_types::node::{NodeSpec, VM_EXPORT};
use opl_types::resource::Resource;
use opl_types::storage::{
    PoolBackend, StoragePool, StoragePoolSpec, Volume, is_shared, pool_state_on, pool_usable_on,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use opl_types::vm_export::{
    DEFAULT_HW_VERSION, VmExport, VmExportPhase, VmExportSpec, VmExportStatus,
};
use ring::{hkdf, hmac};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::backups::{Dest, backup_node, check_online, find_pool, node_name, now, vm_disks};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{check_name, lookup};
use crate::snapshots::check_not_busy;
use crate::state::AppState;
use crate::tasks::create_task;
use crate::vms::is_stopped;
use crate::volumes::{check_pool_on, pool_node};

/// 내려받기 링크가 사는 시간(초)
pub const LINK_TTL_SECS: u64 = 600;
/// 링크 서명 키를 클러스터 CA 키에서 뽑는 HKDF info(OIDC 상태 쿠키 키와 다르다)
const LINK_KEY_INFO: &[u8] = b"orbivisor vm-export download link v1";
/// 내보내기 풀에 남길 여유(VM 디스크 합에 더한다)
const SPACE_RESERVE: u64 = 1 << 30;
/// 다른 노드의 흐름 머리를 기다리는 시간
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportVmRequest {
    /// 파일을 둘 풀(dir·NFS)
    pub pool: String,
    /// 내보내기 이름(클러스터에서 고유). 없으면 `{VM 이름}-{yyyymmdd-HHMMSS}`(UTC)다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub name: Option<String>,
    /// OVF `vmw:osType`(VMware guestId, 예: `ubuntu64Guest`). 없으면 가져온 VM의 원본 값, 그것도 없으면 `otherGuest64`다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub guest_os: Option<String>,
    /// VMware 하드웨어 버전(`vmx-N`, 7~21, 기본 14 — ESXi 6.7 이상)
    #[serde(default)]
    #[schema(nullable = false)]
    pub hw_version: Option<u8>,
    /// NIC의 MAC 주소를 그대로 적는다(`addressType=manual`).
    #[serde(default)]
    pub keep_macs: bool,
}

/// 내보내기 시작의 응답(202)
#[derive(Debug, Serialize, ToSchema)]
pub struct VmExportStarted {
    pub export: Resource<VmExportSpec, VmExportStatus>,
    pub task: Resource<TaskSpec, TaskStatus>,
}

/// 내려받을 꼴
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// 모든 파일을 묶은 OVA(ustar)
    #[default]
    Ova,
    /// OVF 폴더의 파일 하나(`file`)
    Ovf,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ExportDownloadQuery {
    /// `ova`(기본) 또는 `ovf`(파일 하나 — `file`이 든다)
    #[serde(default)]
    #[param(inline)]
    pub format: ExportFormat,
    /// `ovf`에서 받을 파일 이름(`status.files`의 하나)
    pub file: Option<String>,
}

/// 파일을 둘 수 있는 풀(dir·NFS)
pub(crate) fn is_file_pool(pool: &StoragePool) -> bool {
    matches!(
        pool.spec.backend,
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. }
    )
}

/// 풀에 내보내기가 남아 있으면 409 `pool_in_use`(풀 지우기)
pub(crate) fn check_no_exports(state: &AppState, pool: &StoragePool) -> Result<(), ApiError> {
    let (_, exports) = state.cluster.list::<VmExportSpec>()?;
    match exports.iter().find(|e| e.spec.pool == pool.name) {
        Some(e) => Err(ApiError::conflict(
            "pool_in_use",
            format!("pool {:?} holds VM export {:?}", pool.name, e.name),
        )),
        None => Ok(()),
    }
}

/// 내보내기 `id`를 다루는 끝나지 않은 Task(쓰기·지우기)가 하는 일
fn export_task(state: &AppState, id: Uuid) -> Result<Option<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .find_map(|t| match t.spec.op {
            TaskOp::VmExport { export } if export == id => Some("being written"),
            TaskOp::VmExportDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        }))
}

/// 내보낼 디스크 공간: 볼륨마다 할당된 크기(모르면 가상 크기)의 합 + 1 GiB. 노드가 알린 풀의 남은 공간이 모자라면 507
/// `insufficient_storage`(남은 공간을 모르면 막지 않는다 — Task가 쓰다 실패한다).
fn check_space(
    state: &AppState,
    pool: &StoragePool,
    node: Uuid,
    disks: &[(Volume, StoragePool)],
) -> Result<(), ApiError> {
    let need = disks
        .iter()
        .map(|(v, _)| v.status.allocated_bytes.unwrap_or(v.spec.size_bytes))
        .sum::<u64>()
        .saturating_add(SPACE_RESERVE);
    let Some(n) = state.cluster.get::<NodeSpec>(node)? else {
        return Ok(());
    };
    match pool_state_on(pool, &n) {
        Some(s) if s.total_bytes > 0 && s.available_bytes < need => Err(ApiError::new(
            StatusCode::INSUFFICIENT_STORAGE,
            "insufficient_storage",
            format!(
                "storage pool {:?} has {} bytes free; exporting needs about {need}",
                pool.name, s.available_bytes
            ),
        )),
        _ => Ok(()),
    }
}

/// VM을 OVF 패키지로 내보내는 `pending` 내보내기와 그것을 쓸 Task를 만든다(202 `{export, task}`). 모든 노드가 `vm-export`를
/// 알아야 하고(409 `cluster_upgrading`), VM이 꺼져 있어야 하며(409 `vm_running` — 템플릿도 된다), 켜고 끄는 중이면 409 `vm_busy`,
/// 풀은 dir·NFS여야 한다(400 `pool_not_file`). 풀의 `storage.allocate`가 든다(403). Task는 VM 디스크와 풀을 함께 읽고 쓸 수 있는
/// 노드에서 돈다(없으면 409 `no_node`·`pool_not_on_node`). 디스크 합 + 1 GiB가 풀에 없으면 507 `insufficient_storage`. 이름이
/// 겹치면 409 `name_taken`. 쓰는 동안 VM 켜기·디스크 바꾸기는 409 `vm_exporting`이다.
#[utoipa::path(post, path = "/vms/{key}/export", tag = "vm-exports",
    params(("key" = String, Path, description = "VM id or name")),
    request_body = ExportVmRequest,
    responses((status = 202, body = VmExportStarted), (status = "4XX", body = crate::error::ErrorBody),
        (status = 507, body = crate::error::ErrorBody)))]
pub async fn export_vm(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<ExportVmRequest>,
) -> Result<(StatusCode, Json<VmExportStarted>), ApiError> {
    require_feature(&state.cluster, VM_EXPORT)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let mut spec = VmExportSpec {
        vm: vm.id,
        pool: req.pool.clone(),
        guest_os: req.guest_os.clone(),
        hw_version: req.hw_version.unwrap_or(DEFAULT_HW_VERSION),
        keep_macs: req.keep_macs,
        node: Uuid::nil(),
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    let name = req
        .name
        .clone()
        .unwrap_or_else(|| default_backup_name(&vm.name, now()));
    check_name(&name)?;
    let pool = find_pool(&state, &req.pool)?;
    audit.detail(format!("vm export {} → pool {}", vm.name, pool.name));
    state.authz.require_pool(&principal, &pool.name)?;
    if !is_file_pool(&pool) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "pool_not_file",
            format!(
                "storage pool {:?} cannot hold files; export to a dir or NFS pool",
                pool.name
            ),
        ));
    }
    check_not_busy(&state, &vm)?;
    if !is_stopped(&vm) {
        return Err(ApiError::conflict(
            "vm_running",
            format!("VM {:?} is running; stop it to export it", vm.name),
        ));
    }
    let disks = vm_disks(&state, &vm)?;
    let node = backup_node(&state, &vm, &Dest::Pool(pool.clone()), &disks)?;
    check_pool_on(&state, &pool, node, || {
        format!(
            "storage pool {:?} is not available on the node holding VM {:?}",
            pool.name, vm.name
        )
    })?;
    check_space(&state, &pool, node, &disks)?;
    spec.node = node;
    let export: VmExport = Resource::new(name, spec);
    // 같은 이름이 있으면 저장소가 거부한다(409 name_taken).
    state.cluster.create(&export).await?;
    let op = TaskOp::VmExport { export: export.id };
    match create_task(&state.cluster, op, vm.id, node).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(VmExportStarted { export, task }))),
        Err(e) => {
            if let Err(d) = state.cluster.delete::<VmExportSpec>(export.id, None).await {
                tracing::warn!(export = %export.id, error = %d, "cannot remove export without a task");
            }
            Err(e)
        }
    }
}

/// 내보내기 지우기 Task를 돌릴 노드: 풀이 없어졌으면 이 노드(리소스만 지운다), 공유가 아닌 풀은 파일을 쓴 노드(오프라인이면
/// 409 `node_offline`), 공유 풀은 그 풀을 쓸 수 있는 노드
fn delete_node(state: &AppState, export: &VmExport) -> Result<Uuid, ApiError> {
    let Some(pool) = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&export.spec.pool)?
    else {
        return Ok(state.node);
    };
    if is_shared(&pool) {
        return pool_node(state, &pool);
    }
    let node = export.spec.node;
    check_online(state, node)?;
    check_pool_on(state, &pool, node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding export {:?}",
            pool.name,
            node_name(state, node).unwrap_or_else(|_| node.to_string()),
            export.name
        )
    })?;
    Ok(node)
}

/// 내보내기의 파일과 리소스를 지울 Task를 돌려준다(202). 실패한 내보내기도 지운다. 쓰거나 지우는 Task가 끝나지 않았으면 409
/// `export_busy`, 파일이 있는 노드가 오프라인이면 409 `node_offline`.
#[utoipa::path(delete, path = "/vm-exports/{key}", tag = "vm-exports",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_export(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let export = lookup::<VmExportSpec>(&state.cluster, &key)?;
    audit.detail(format!("vm-export delete {}", export.name));
    if let Some(doing) = export_task(&state, export.id)? {
        return Err(ApiError::conflict(
            "export_busy",
            format!(
                "export {:?} is {doing}; try again when the task has finished",
                export.name
            ),
        ));
    }
    let node = delete_node(&state, &export)?;
    let task = create_task(&state.cluster, TaskOp::VmExportDelete, export.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

fn not_ready(export: &VmExport) -> ApiError {
    ApiError::conflict(
        "export_not_ready",
        match export.status.phase {
            VmExportPhase::Failed => format!(
                "export {:?} failed: {}",
                export.name,
                export.status.error.as_deref().unwrap_or("unknown error")
            ),
            _ => format!("export {:?} is still being written", export.name),
        },
    )
}

fn link_expired() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "link_expired",
        "the download link has expired or is not valid; ask for a new one",
    )
}

/// 링크 서명 키(클러스터 CA 키에서 — 모든 노드가 같다)
fn link_key(state: &AppState) -> Result<hmac::Key, ApiError> {
    let info = crate::cluster::cluster_info(state)?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(info.spec.ca_key_pem.as_bytes());
    let okm = prk
        .expand(&[LINK_KEY_INFO], hmac::HMAC_SHA256)
        .map_err(|_| ApiError::internal("cannot derive the link key"))?;
    Ok(hmac::Key::from(okm))
}

fn link_message(export: Uuid, user: Uuid, expires: u64) -> String {
    format!("{export}.{user}.{expires}")
}

/// 링크 토큰 `{만료 초}.{사용자 id}.{HMAC}`
fn sign_link(key: &hmac::Key, export: Uuid, user: Uuid, expires: u64) -> String {
    use base64::Engine as _;
    let tag = hmac::sign(key, link_message(export, user, expires).as_bytes());
    format!(
        "{expires}.{}.{}",
        user.simple(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tag.as_ref())
    )
}

/// 토큰을 확인한다: 다른 사용자의 것이면 403, 틀리거나 지났으면 410 `link_expired`
fn check_link(
    key: &hmac::Key,
    token: &str,
    export: Uuid,
    user: Uuid,
    now: u64,
) -> Result<(), ApiError> {
    use base64::Engine as _;
    let mut parts = token.splitn(3, '.');
    let (Some(expires), Some(owner), Some(tag)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(link_expired());
    };
    let (Ok(expires), Ok(owner)) = (expires.parse::<u64>(), Uuid::try_parse(owner)) else {
        return Err(link_expired());
    };
    let Ok(tag) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(tag) else {
        return Err(link_expired());
    };
    if hmac::verify(key, link_message(export, owner, expires).as_bytes(), &tag).is_err() {
        return Err(link_expired());
    }
    if owner != user {
        return Err(ApiError::forbidden());
    }
    if expires <= now {
        return Err(link_expired());
    }
    Ok(())
}

/// 내려받기 링크(§12.3): 10분 사는 서명 링크를 만든다(요청한 사용자에 묶임). 브라우저·CLI가 `GET url`로 받는다(같은 사용자의
/// 인증이 다시 필요하다). 다 쓰지 않은 내보내기는 409 `export_not_ready`.
#[utoipa::path(post, path = "/vm-exports/{key}/download-link", tag = "vm-exports",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = DownloadLink), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn export_download_link(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<DownloadLink>, ApiError> {
    let export = lookup::<VmExportSpec>(&state.cluster, &key)?;
    audit.detail(format!("vm-export download-link {}", export.name));
    if export.status.phase != VmExportPhase::Ready {
        return Err(not_ready(&export));
    }
    let token = sign_link(
        &link_key(&state)?,
        export.id,
        principal.user,
        now() + LINK_TTL_SECS,
    );
    Ok(Json(DownloadLink {
        url: format!("/vm-exports/{}/download/{token}", export.id),
        expires_in: LINK_TTL_SECS,
    }))
}

/// 이 노드에서 내보내기 파일이 보이는지: 쓴 노드이거나, 공유 풀이 이 노드에서 준비됐다.
fn files_here(state: &AppState, export: &VmExport) -> Result<bool, ApiError> {
    if export.spec.node == state.node {
        return Ok(true);
    }
    let Some(pool) = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&export.spec.pool)?
    else {
        return Ok(false);
    };
    let Some(me) = state.cluster.get::<NodeSpec>(state.node)? else {
        return Ok(false);
    };
    Ok(is_shared(&pool) && pool_usable_on(&pool, &me))
}

/// 다른 노드가 낼 수 있는 오류 코드(문자열을 API 오류 코드로 되돌린다)
const CODES: &[&str] = &[
    "not_found",
    "export_not_ready",
    "pool_unavailable",
    "file_not_found",
    "cluster_upgrading",
    "node_unreachable",
    "forbidden",
];

fn from_fault(f: FileRestoreFault) -> ApiError {
    let status = StatusCode::from_u16(f.status).unwrap_or(StatusCode::BAD_GATEWAY);
    let code = CODES
        .iter()
        .find(|c| **c == f.code)
        .copied()
        .unwrap_or("export_error");
    ApiError::new(status, code, f.message)
}

fn fault(e: ApiError) -> FileRestoreFault {
    FileRestoreFault::new(e.status.as_u16(), e.code, e.message)
}

/// `3.1 GiB`
fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    match u {
        0 => format!("{n} B"),
        _ => format!("{v:.1} {}", UNITS[u]),
    }
}

/// OVA 파일 이름(`{VM}.ova` — 서술자 이름에서)
fn ova_name(export: &VmExport) -> String {
    let stem = export
        .status
        .files
        .first()
        .and_then(|f| f.name.strip_suffix(".ovf"))
        .unwrap_or(&export.name);
    format!("{stem}.ova")
}

/// 이 노드에서 내보내기 파일을 흘린다: 내보내기가 `ready`이고 풀이 이 노드에서 마운트돼 있어야 한다. `file`이면 그 파일
/// 하나, 아니면 OVA다. API 노드와 노드 사이 흐름(`ExportDownload`)이 함께 쓴다.
pub(crate) async fn open_local(
    cluster: &Cluster,
    mount_root: &FsPath,
    req: ExportDownloadRequest,
) -> Result<(DownloadHead, DownloadStream), ApiError> {
    let export = cluster
        .get::<VmExportSpec>(req.export)?
        .ok_or_else(|| ApiError::not_found(format!("vm-export {} not found", req.export)))?;
    if export.status.phase != VmExportPhase::Ready {
        return Err(not_ready(&export));
    }
    let pool = cluster
        .get_by_name::<StoragePoolSpec>(&export.spec.pool)?
        .ok_or_else(|| {
            ApiError::conflict(
                "pool_unavailable",
                format!("storage pool {:?} was deleted", export.spec.pool),
            )
        })?;
    let root = opl_driver::check_pool_root(&pool, mount_root).map_err(|e| {
        ApiError::conflict(
            "pool_unavailable",
            format!("storage pool {:?}: {e}", pool.name),
        )
    })?;
    let dir: PathBuf = root.join(VmExportSpec::dir(export.id));
    let parts: Vec<tar::Part> = export
        .status
        .files
        .iter()
        .filter(|f| req.file.as_ref().is_none_or(|n| *n == f.name))
        .map(|f| tar::Part {
            name: f.name.clone(),
            path: dir.join(&f.name),
            size: f.size,
        })
        .collect();
    let (name, size, format, tar_it, content_type) = match &req.file {
        Some(file) => {
            let part = parts.first().ok_or_else(|| {
                ApiError::new(
                    StatusCode::NOT_FOUND,
                    "file_not_found",
                    format!("export {:?} has no file {file:?}", export.name),
                )
            })?;
            (
                part.name.clone(),
                part.size,
                "ovf",
                false,
                if part.name.ends_with(".ovf") {
                    "application/xml"
                } else {
                    "application/octet-stream"
                },
            )
        }
        None => (
            ova_name(&export),
            tar::ova_size(parts.iter().map(|p| (p.name.as_str(), p.size))),
            "ova",
            true,
            "application/x-tar",
        ),
    };
    // 시각은 서술자를 쓴 때(없으면 0)
    let mtime = match parts.first() {
        Some(p) => tokio::fs::metadata(&p.path)
            .await
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs()),
        None => 0,
    };
    let describe = format!(
        "{} ({}, {})",
        export.name,
        format.to_uppercase(),
        human(size)
    );
    let head = DownloadHead {
        name,
        content_type: content_type.into(),
        size: Some(size),
        format: format.into(),
        describe,
    };
    Ok((head, tar::spawn(parts, tar_it, mtime)))
}

/// 노드 사이 흐름 `ExportDownload`의 처리(이 노드의 내보내기 파일)
pub struct ExportFiles {
    pub cluster: Arc<Cluster>,
    pub mount_root: PathBuf,
}

#[async_trait]
impl ExportSource for ExportFiles {
    async fn download(
        &self,
        req: ExportDownloadRequest,
    ) -> Result<(DownloadHead, DownloadStream), FileRestoreFault> {
        open_local(&self.cluster, &self.mount_root, req)
            .await
            .map_err(fault)
    }
}

/// 흐름이 끝날 때(끊겨도) 적는 감사 항목
struct DownloadAudit {
    state: Arc<AppState>,
    principal: Principal,
    ip: std::net::IpAddr,
    path: String,
    started: Instant,
    detail: String,
    bytes: u64,
    done: bool,
    failed: Option<String>,
}

impl DownloadAudit {
    fn finish(&mut self) {
        self.done = true;
    }
}

impl Drop for DownloadAudit {
    fn drop(&mut self) {
        let detail = match (&self.done, &self.failed) {
            (true, _) => self.detail.clone(),
            (false, Some(e)) => {
                format!("{} — failed after {}: {e}", self.detail, human(self.bytes))
            }
            (false, None) => format!("{} — interrupted after {}", self.detail, human(self.bytes)),
        };
        crate::audit::record_stream(
            &self.state,
            &self.principal,
            self.ip,
            "/vm-exports/{key}/download/{token}",
            &self.path,
            200,
            self.started,
            detail,
        );
    }
}

/// 링크로 내려받는다(GET — 브라우저 내려받기). 링크를 만든 사용자만(403), 지났거나 틀린 링크는 410 `link_expired`. 기본은 OVA
/// (`application/x-tar`, `{VM}.ova`, 정확한 `Content-Length` — 서술자 → 매니페스트 → 디스크), `format=ovf&file=NAME`은 파일
/// 하나다. 파일이 이 노드에 없으면 쓴 노드에서 받아 넘긴다(닿지 않으면 503 `node_unreachable`). 도중의 읽기 실패는 연결을
/// 끊는다(받은 파일이 잘렸음을 클라이언트가 안다). 감사는 흐름이 끝날 때 적는다.
#[utoipa::path(get, path = "/vm-exports/{key}/download/{token}", tag = "vm-exports",
    params(("key" = String, Path, description = "id or name"), ("token" = String, Path), ExportDownloadQuery),
    responses((status = 200, content_type = "application/x-tar", description = "the OVA, or one file of the OVF folder"),
        (status = "4XX", body = crate::error::ErrorBody), (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn download_export(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    crate::limit::ClientIp(ip): crate::limit::ClientIp,
    Path((key, token)): Path<(String, String)>,
    Query(q): Query<ExportDownloadQuery>,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    let export = lookup::<VmExportSpec>(&state.cluster, &key)?;
    check_link(&link_key(&state)?, &token, export.id, principal.user, now())?;
    if export.status.phase != VmExportPhase::Ready {
        return Err(not_ready(&export));
    }
    let file = match (q.format, q.file) {
        (ExportFormat::Ova, None) => None,
        (ExportFormat::Ovf, Some(file)) => Some(file),
        (ExportFormat::Ova, Some(_)) => {
            return Err(ApiError::bad_request("file is only for format=ovf"));
        }
        (ExportFormat::Ovf, None) => {
            return Err(ApiError::bad_request("format=ovf needs file"));
        }
    };
    let req = ExportDownloadRequest {
        export: export.id,
        file,
    };
    let (head, rx) = if files_here(&state, &export)? {
        open_local(&state.cluster, &state.mount_root, req).await?
    } else {
        let node = export.spec.node;
        let unreachable = |why: &str| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "node_unreachable",
                format!("the node holding the export did not answer: {why}"),
            )
        };
        if state
            .cluster
            .get::<NodeSpec>(node)?
            .is_some_and(|n| !n.status.online)
        {
            return Err(unreachable("the node is offline"));
        }
        let peers = state
            .peers
            .clone()
            .ok_or_else(|| unreachable("no node-to-node connection"))?;
        let addr = state
            .cluster
            .members()
            .addr(NodeId(node))
            .map(str::to_string)
            .ok_or_else(|| unreachable("not a cluster member"))?;
        peers
            .export_download(NodeId(node), &addr, req, OPEN_TIMEOUT)
            .await
            .map_err(from_fault)?
    };
    let audit = DownloadAudit {
        state: state.clone(),
        principal,
        ip,
        path: format!("/vm-exports/{}/download/-", export.id),
        started,
        detail: format!("vm-export download {}", head.describe),
        bytes: 0,
        done: false,
        failed: None,
    };
    let body = futures_util::stream::unfold((rx, audit), |(mut rx, mut audit)| async move {
        match rx.recv().await {
            Some(Ok(DownloadPiece::Data(d))) => {
                audit.bytes += d.len() as u64;
                Some((Ok::<_, std::io::Error>(d), (rx, audit)))
            }
            Some(Ok(DownloadPiece::Done { .. })) => {
                audit.finish();
                None
            }
            Some(Err(e)) => {
                audit.failed = Some(e.clone());
                Some((Err(std::io::Error::other(e)), (rx, audit)))
            }
            None => None,
        }
    });
    let mut resp = Body::from_stream(body).into_response();
    let headers = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&head.content_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&head.name)) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(size) = head.size {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_bound_to_export_user_and_time() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"k");
        let (export, user, other) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
        let token = sign_link(&key, export, user, 1000);
        assert_eq!(
            check_link(&key, &token, export, user, 999).map_err(|e| e.status),
            Ok(())
        );
        let code = |r: Result<(), ApiError>| r.unwrap_err().code;
        // 지났다
        assert_eq!(
            code(check_link(&key, &token, export, user, 1000)),
            "link_expired"
        );
        // 다른 사용자
        assert_eq!(
            code(check_link(&key, &token, export, other, 999)),
            "forbidden"
        );
        // 다른 내보내기, 고친 만료, 다른 키, 엉뚱한 글
        assert_eq!(
            code(check_link(&key, &token, Uuid::now_v7(), user, 999)),
            "link_expired"
        );
        let forged = token.replacen("1000", "9000", 1);
        assert_eq!(
            code(check_link(&key, &forged, export, user, 999)),
            "link_expired"
        );
        let other_key = hmac::Key::new(hmac::HMAC_SHA256, b"other");
        assert_eq!(
            code(check_link(&other_key, &token, export, user, 999)),
            "link_expired"
        );
        for bad in [
            "",
            "x",
            "1.2",
            "1.zz.abc",
            &format!("1000.{}.!!", user.simple()),
        ] {
            assert_eq!(
                code(check_link(&key, bad, export, user, 999)),
                "link_expired",
                "{bad}"
            );
        }
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(3 << 30), "3.0 GiB");
    }
}
