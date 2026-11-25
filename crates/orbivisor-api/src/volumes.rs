//! 볼륨 생성·가져오기·삭제. 파일 작업은 풀을 쓸 수 있는 노드의 Task 실행기가 한다.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, StatusCode};
use futures_util::StreamExt;
use orbivisor_cluster::{Cluster, ClusterError};
use orbivisor_types::authz::Privilege;
use orbivisor_types::cloud_init::PASSWORD_MASK;
use orbivisor_types::node::{INSTALL_MEDIA, NodeSpec, TEMPLATES};
use orbivisor_types::resource::Resource;
use orbivisor_types::runtime::ImageFormat;
use orbivisor_types::snapshot::SnapshotSpec;
use orbivisor_types::storage::{
    PoolBackend, SourceFormat, StoragePool, StoragePoolSpec, Volume, VolumeContent, VolumePhase,
    VolumeSpec, is_shared, pool_error_on, pool_holds_isos, pool_listed_on, pool_usable_on,
};
use orbivisor_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use orbivisor_types::vm::{Vm, VmSpec};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, check_name, lookup};
use crate::vms::{is_stopped, vm_uses_volume, volume_in_use};

/// `/volumes/import`·`/volumes/download` 경로와 겹치므로 볼륨 이름으로 쓸 수 없다.
const RESERVED_NAMES: [&str; 2] = ["import", "download"];

pub(crate) fn check_volume_name(name: &str) -> Result<(), ApiError> {
    check_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        return Err(ApiError::bad_request(format!(
            "the name {name} is reserved"
        )));
    }
    Ok(())
}
use crate::snapshots::{check_no_snapshot_task, check_not_busy, count_snapshots, vm_placement};
use crate::state::AppState;
use crate::tasks::create_task;

/// 볼륨 파일 작업을 실행할 노드. 이 노드에서 풀을 쓸 수 있으면 이 노드, 아니면 풀을 쓸 수 있는
/// 온라인 노드 가운데 id가 가장 작은 노드다. RBD·공유 LVM 풀은 이 노드를 앞세우지 않고 늘 id가 가장
/// 작은 노드다: 이미지(LV)를 바꾸는 명령이 아직 돌고 있다는 표시가 노드마다 따로이므로 한 볼륨의
/// Task가 같은 노드에서 돌게 한다. 그런 노드가 없으면 409 `pool_unavailable`.
pub(crate) fn pool_node(state: &AppState, pool: &StoragePool) -> Result<Uuid, ApiError> {
    let lowest = matches!(
        pool.spec.backend,
        PoolBackend::Rbd { .. } | PoolBackend::LvmShared { .. }
    );
    let prefer = (!lowest).then_some(state.node);
    usable_node(&state.cluster, pool, prefer)?.ok_or_else(|| {
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
pub(crate) fn volume_node(state: &AppState, volume: &Volume) -> Result<Uuid, ApiError> {
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

pub(crate) fn find_pool(state: &AppState, name: &str) -> Result<StoragePool, ApiError> {
    state
        .cluster
        .get_by_name::<StoragePoolSpec>(name)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {name:?} not found")))
}

/// 볼륨이 raw 블록 장치나 이미지인 블록 풀의 종류(`lvm-thin`, `zfs`, `rbd`, `lvm-shared`). 파일
/// 풀(dir, nfs)이면 `None`.
pub(crate) fn block_pool_kind(pool: &StoragePool) -> Option<&'static str> {
    match pool.spec.backend {
        PoolBackend::LvmThin { .. } => Some("lvm-thin"),
        PoolBackend::Zfs { .. } => Some("zfs"),
        PoolBackend::Rbd { .. } => Some("rbd"),
        PoolBackend::LvmShared { .. } => Some("lvm-shared"),
        PoolBackend::Dir { .. } | PoolBackend::Nfs { .. } => None,
    }
}

/// 풀이 `format` 볼륨을 담을 수 있는지. 블록 풀(LVM-thin, ZFS, RBD, 공유 LVM)은 raw만 담는다.
pub(crate) fn check_pool_format(pool: &StoragePool, format: ImageFormat) -> Result<(), ApiError> {
    let Some(kind) = block_pool_kind(pool) else {
        return Ok(());
    };
    if format != ImageFormat::Raw {
        return Err(ApiError::bad_request(format!(
            "{kind} pools hold raw volumes"
        )));
    }
    Ok(())
}

/// 형식을 적지 않았을 때의 볼륨 형식: 블록 풀([`block_pool_kind`])은 raw, 그 밖은 스냅샷을 담을 수 있는
/// qcow2
pub(crate) fn default_format(pool: &StoragePool) -> ImageFormat {
    if block_pool_kind(pool).is_some() {
        ImageFormat::Raw
    } else {
        ImageFormat::Qcow2
    }
}

/// ISO 볼륨의 규칙(계획 57): dir·NFS 풀에만 두고 형식과 원본 형식은 raw다. `how`는 원본 형식 문구의 동사
/// (`upload`·`download`). 만들 형식(raw)을 돌려준다.
fn check_iso(
    pool: &StoragePool,
    format: Option<ImageFormat>,
    source_format: Option<SourceFormat>,
    how: &str,
) -> Result<ImageFormat, ApiError> {
    if !pool_holds_isos(&pool.spec.backend) {
        return Err(ApiError::bad_request(
            "ISO images can be kept only in dir or nfs pools",
        ));
    }
    if format.is_some_and(|f| f != ImageFormat::Raw) {
        return Err(ApiError::bad_request("ISO volumes are raw"));
    }
    if source_format.is_some_and(|f| f != SourceFormat::Raw) {
        return Err(ApiError::bad_request(format!("{how} ISO images as raw")));
    }
    Ok(ImageFormat::Raw)
}

/// 빈 디스크 볼륨 만들기(`POST /volumes`)의 검사: 이름, 디스크 내용, 크기, `base` 없음, 풀이 있음(400)·`storage.allocate`,
/// 풀이 담는 형식, 파일을 만들 노드(409 `pool_unavailable`). 풀과 그 노드를 돌려준다. VM 미리 확인(계획 71)의 만들
/// 볼륨도 이것으로 본다.
pub(crate) fn check_new_volume(
    state: &AppState,
    principal: &Principal,
    req: &CreateRequest<VolumeSpec>,
) -> Result<(StoragePool, Uuid), ApiError> {
    check_volume_name(&req.name)?;
    if !req.spec.content.is_disk() {
        return Err(ApiError::bad_request(
            "create ISO volumes by uploading or downloading an image",
        ));
    }
    if req.spec.size_bytes == 0 {
        return Err(ApiError::bad_request("size_bytes must be greater than 0"));
    }
    // 연결 복제(계획 58)는 배포 Task만 만든다: 빈 볼륨의 `base`는 거짓 기록이고(기반 지우기·되돌리기를 막는다) 옛 노드는
    // 이 칸을 풀지 못한다.
    if req.spec.base.is_some() {
        return Err(ApiError::bad_request(
            "base is set only by deploying from a template",
        ));
    }
    let pool = find_pool(state, &req.spec.pool)?;
    state
        .authz
        .require(principal, Privilege::StorageAllocate, &pool)?;
    check_pool_format(&pool, req.spec.format)?;
    let node = pool_node(state, &pool)?;
    Ok((pool, node))
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
    let (_, node) = check_new_volume(&state, &principal, &req)?;
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
    /// 내용 종류(기본 disk). iso는 dir·nfs 풀에 raw로 둔다.
    #[serde(default)]
    #[param(inline)]
    pub content: VolumeContent,
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
    /// 내용 종류(기본 disk)
    #[serde(default)]
    #[param(inline)]
    pub content: VolumeContent,
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
    let iso = !q.content.is_disk();
    if iso {
        require_feature(&state.cluster, INSTALL_MEDIA)?;
    }
    check_volume_name(&q.name)?;
    let pool = find_pool(state, &q.pool)?;
    state
        .authz
        .require(principal, Privilege::StorageAllocate, &pool)?;
    let format = if iso {
        check_iso(&pool, q.format, Some(q.source_format), "upload")?
    } else {
        let format = q.format.unwrap_or_else(|| default_format(&pool));
        check_pool_format(&pool, format)?;
        format
    };
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
    check_upload_space(state, size).await?;
    Ok((pool, format))
}

/// 업로드 디렉터리에 `size`바이트를 받고도 [`UPLOAD_RESERVE_BYTES`]가 남는지 본다. 아니면 507.
pub(crate) async fn check_upload_space(state: &AppState, size: u64) -> Result<(), ApiError> {
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
    Ok(())
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
        content: q.content,
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
            content: q.content,
            base: None,
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

/// `POST /volumes/download`: URL에서 이미지를 받아 볼륨을 만든다(계획 57).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VolumeDownloadRequest {
    pub name: String,
    pub pool: String,
    /// 받을 http·https URL. 작업 노드가 닿을 수 있어야 한다.
    pub url: String,
    /// 내용 종류(기본 disk). iso는 dir·nfs 풀에 raw로 둔다.
    #[serde(default)]
    pub content: VolumeContent,
    /// 받은 파일의 SHA-256(16진 64자). 있으면 다르면 실패한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub sha256: Option<String>,
    /// 받은 파일의 형식(디스크만, 기본 raw)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub source_format: Option<SourceFormat>,
    /// 만들 볼륨의 형식 (생략하면 풀 종류로: 블록 풀은 raw, 그 밖은 qcow2. ISO는 늘 raw)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub format: Option<ImageFormat>,
    /// 받은 파일의 SHA-512(16진 128자, 계획 58). `sha256`·`checksum_url`과 함께 쓰지 않는다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub sha512: Option<String>,
    /// 체크섬 파일(`SHA256SUMS`·`SHA512SUMS`·BSD `CHECKSUM` 모양)의 http·https URL(계획 58). 받기 전에 이 파일에서
    /// 이미지 URL의 파일 이름 줄을 찾아 그 해시로 확인한다. `sha256`·`sha512`와 함께 쓰지 않는다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub checksum_url: Option<String>,
}

/// URL의 사용자 정보(`user:password@`)를 `***@`로, 쿼리 문자열(서명·토큰이 흔히 든다)을 `?***`로 가린다. 감사
/// 설명, Task 오류 문구, 응답·watch로 내보내는 받기 Task에 쓴다(설정 이력도 같은 가림 — 계획 73 리뷰 1).
pub(crate) use orbivisor_types::image_catalog::redact_source_url as redact_url;

/// 응답으로 낼 Task: 받기 Task의 URL·체크섬 파일 URL을 가리고([`redact_url`]), 템플릿 초안(받기)과 배포 덮어쓰기
/// (복제)의 cloud-init 비밀번호(해시)를 [`PASSWORD_MASK`]로 가린다(계획 58). 저장은 그대로다(Task 실행은 원래 값을
/// 쓴다).
pub(crate) fn public_task(task: &mut Task, writer: bool) {
    let ci = match &mut task.spec.op {
        TaskOp::VolumeDownload {
            url,
            checksum_url,
            template,
            ..
        } => {
            *url = redact_url(url);
            if let Some(sums) = checksum_url {
                *sums = redact_url(sums);
            }
            template.as_mut().and_then(|t| t.cloud_init.as_mut())
        }
        TaskOp::VmClone { overrides, .. } => overrides.as_mut().and_then(|o| o.cloud_init.as_mut()),
        _ => None,
    };
    let Some(ci) = ci else { return };
    if let Some(password) = ci.password.as_mut() {
        *password = PASSWORD_MASK.into();
    }
    // VM 쓰기 권한이 없으면 user-data·vendor-data도 가린다(VM 스펙과 같음).
    if !writer {
        for data in [&mut ci.user_data, &mut ci.vendor_data]
            .into_iter()
            .flatten()
        {
            *data = PASSWORD_MASK.into();
        }
    }
}

/// 소문자 16진 `len`자로 바꾼 해시(`field`는 오류 문구의 이름). 아니면 400.
fn parse_hex(field: &str, sum: &str, len: usize) -> Result<String, ApiError> {
    if sum.len() != len || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request(format!(
            "{field} must be {len} hex digits"
        )));
    }
    Ok(sum.to_ascii_lowercase())
}

/// http·https URL인지. 아니면 400(`field must be an http or https URL`).
pub(crate) fn check_http_url(field: &str, url: &str) -> Result<(), ApiError> {
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") => Ok(()),
        _ => Err(ApiError::bad_request(format!(
            "{field} must be an http or https URL"
        ))),
    }
}

/// 받은 파일을 확인할 방법: 고정 SHA-256·SHA-512(소문자로)나 체크섬 파일 URL 가운데 하나(둘 이상이면 400).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DownloadCheck {
    pub sha256: Option<String>,
    pub sha512: Option<String>,
    pub checksum_url: Option<String>,
}

impl DownloadCheck {
    pub(crate) fn parse(
        sha256: Option<&str>,
        sha512: Option<&str>,
        checksum_url: Option<&str>,
    ) -> Result<Self, ApiError> {
        let given = [sha256.is_some(), sha512.is_some(), checksum_url.is_some()];
        if given.into_iter().filter(|g| *g).count() > 1 {
            return Err(ApiError::bad_request(
                "give only one of sha256, sha512 and checksum_url",
            ));
        }
        if let Some(url) = checksum_url {
            check_http_url("checksum_url", url)?;
        }
        Ok(Self {
            sha256: sha256.map(|s| parse_hex("sha256", s, 64)).transpose()?,
            sha512: sha512.map(|s| parse_hex("sha512", s, 128)).transpose()?,
            checksum_url: checksum_url.map(str::to_string),
        })
    }

    /// 감사 설명에 덧붙일 말(해시는 앞 4자만, URL은 가림)
    pub(crate) fn describe(&self) -> String {
        match (&self.sha256, &self.sha512, &self.checksum_url) {
            (Some(sum), _, _) => format!(" (sha256 {}…)", &sum[..4]),
            (_, Some(sum), _) => format!(" (sha512 {}…)", &sum[..4]),
            (_, _, Some(url)) => format!(" (checksum file {})", redact_url(url)),
            _ => String::new(),
        }
    }
}

/// 받기 Task가 채울 볼륨 리소스(`pending`, 크기 0 — 가져온 뒤 실제 이미지 크기가 된다)를 풀을 쓸 수 있는 노드
/// ([`pool_node`])에 만들고 `op`(받기)로 Task를 만든다. 이름이 쓰였으면 409 `name_taken`. 응답용으로 가린 Task다.
pub(crate) async fn start_download(
    state: &AppState,
    name: String,
    pool: &StoragePool,
    format: ImageFormat,
    content: VolumeContent,
    op: TaskOp,
) -> Result<Task, ApiError> {
    if state.cluster.get_by_name::<VolumeSpec>(&name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("volume {name:?} already exists"),
        ));
    }
    let node = pool_node(state, pool)?;
    let mut volume: Volume = Resource::new(
        name,
        VolumeSpec {
            pool: pool.name.clone(),
            size_bytes: 0,
            format,
            content,
            base: None,
        },
    );
    volume.status.node = Some(node);
    state.cluster.create(&volume).await?;
    let mut task = create_task(&state.cluster, op, volume.id, node).await?;
    // 요청한 주체가 준 값이다.
    public_task(&mut task, true);
    Ok(task)
}

/// URL에서 이미지를 받아 볼륨을 채울 Task를 돌려준다(202). 풀을 쓸 수 있는 노드(`pool_node`)가 받는다.
/// 크기는 받아 가져온 뒤 실제 이미지 크기가 된다. 모든 노드가 `install-media`를 알아야 한다(새 Task).
/// 확인은 `sha256`·`sha512`·`checksum_url` 가운데 하나이고(둘 이상이면 400), `sha512`·`checksum_url`은 모든 노드가
/// `templates`를 알아야 한다(409 `cluster_upgrading`).
#[utoipa::path(post, path = "/volumes/download", tag = "storage",
    request_body = VolumeDownloadRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn download_volume(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<VolumeDownloadRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, INSTALL_MEDIA)?;
    // 새 확인 칸은 옛 노드가 풀지 못한다(계획 58). 기능 검사는 검증보다 먼저다.
    if req.sha512.is_some() || req.checksum_url.is_some() {
        require_feature(&state.cluster, TEMPLATES)?;
    }
    check_volume_name(&req.name)?;
    check_http_url("url", &req.url)?;
    let check = DownloadCheck::parse(
        req.sha256.as_deref(),
        req.sha512.as_deref(),
        req.checksum_url.as_deref(),
    )?;
    let pool = find_pool(&state, &req.pool)?;
    state
        .authz
        .require(&principal, Privilege::StorageAllocate, &pool)?;
    let format = if req.content.is_disk() {
        let format = req.format.unwrap_or_else(|| default_format(&pool));
        check_pool_format(&pool, format)?;
        format
    } else {
        check_iso(&pool, req.format, req.source_format, "download")?
    };
    let detail = format!(
        "volume download {} from {}{}",
        req.name,
        redact_url(&req.url),
        check.describe()
    );
    let op = TaskOp::VolumeDownload {
        url: req.url,
        sha256: check.sha256,
        source_format: req.source_format.unwrap_or(SourceFormat::Raw),
        sha512: check.sha512,
        checksum_url: check.checksum_url,
        template: None,
    };
    let task = start_download(&state, req.name, &pool, format, req.content, op).await?;
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 올린 파일이 다 들어와도 데이터 디렉터리(Raft 저장소가 있다)에 남겨 둘 여유 공간
pub(crate) const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;

pub(crate) fn content_length(headers: &HeaderMap) -> Result<u64, ApiError> {
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
pub(crate) async fn receive(path: &FsPath, body: Body, length: u64) -> Result<(), ApiError> {
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
/// 스냅샷의 VM 스펙이 가리키면 409 `volume_in_snapshot`, 템플릿의 디스크면 409 `volume_is_template`, 연결 복제의
/// 기반이면 409 `volume_has_clones`(복제를 먼저 지우거나 평탄화한다).
#[utoipa::path(delete, path = "/volumes/{key}", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_volume(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    if volume.status.phase != VolumePhase::Ready {
        return Err(ApiError::conflict(
            "volume_not_ready",
            format!("volume {:?} is still being created", volume.name),
        ));
    }
    // 이 노드에서 같은 ISO를 CD에 넣는 요청과 엇갈리지 않게 확인과 Task 만들기를 VM 쓰기와 함께 잠근다(다른 노드와의
    // 엇갈림은 지우기 Task가 다시 본다).
    let _lock = state.vm_lock.lock().await;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    if let Some(template) = template_of(&vms, &volume.name) {
        return Err(volume_is_template(&volume.name, &template.name));
    }
    let users: Vec<&Vm> = vms
        .iter()
        .filter(|vm| vm_uses_volume(vm, &volume.name))
        .collect();
    if let Some(vm) = users.first() {
        if !volume.spec.content.is_disk() {
            return Err(ApiError::conflict(
                "volume_in_use",
                in_cdrom_message(&volume.name, &users),
            ));
        }
        return Err(volume_in_use(
            vm,
            &volume.name,
            format!("volume {:?} is attached to VM {:?}", volume.name, vm.name),
        ));
    }
    let (_, snapshots) = state.cluster.list::<SnapshotSpec>()?;
    if let Some(s) = snapshots
        .iter()
        .find(|s| spec_refers_to(&s.spec.vm_spec, &volume.name))
    {
        return Err(ApiError::conflict(
            "volume_in_snapshot",
            in_snapshot_message(&volume.name, &s.spec.name, s.spec.vm),
        ));
    }
    // 연결 복제의 기반은 지우지 않는다(계획 58 §4). 다른 노드의 복제 Task와의 엇갈림은 지우기 Task가 다시 본다.
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    let clones = linked_clones(&volumes, &[volume.id]);
    if !clones.is_empty() {
        let mut names: Vec<&str> = clones.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        return Err(ApiError::conflict(
            "volume_has_clones",
            has_clones_message(&volume.name, &names),
        ));
    }
    let node = volume_node(&state, &volume)?;
    let task = create_task(&state.cluster, TaskOp::VolumeDelete, volume.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 스펙이 볼륨을 디스크나 CD-ROM 매체로 가리키는지(스냅샷 되돌리기가 그것으로 켠다)
pub(crate) fn spec_refers_to(spec: &VmSpec, volume: &str) -> bool {
    spec.disks.iter().any(|d| d.volume == volume)
        || spec.cdroms.iter().any(|c| c.iso.as_deref() == Some(volume))
}

/// VM `vm`의 스냅샷 `snapshot`이 가리키는 볼륨을 지울 수 없다는 문구
pub(crate) fn in_snapshot_message(volume: &str, snapshot: &str, vm: Uuid) -> String {
    format!("volume {volume:?} is used by snapshot {snapshot:?} of VM {vm}")
}

/// 이름 목록을 셋까지 적고 나머지는 `…`로 줄인다(연결 복제 문구).
fn short_list(names: &[&str]) -> String {
    let mut list = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
    if names.len() > 3 {
        list.push_str(", …");
    }
    list
}

/// 연결 복제(`clones`, 볼륨 이름)의 기반인 볼륨을 지울 수 없다는 문구(계획 58 `volume_has_clones`)
pub(crate) fn has_clones_message(volume: &str, clones: &[&str]) -> String {
    format!(
        "volume {volume:?} is the base of {} linked clone(s) ({}); delete or flatten them first",
        clones.len(),
        short_list(clones)
    )
}

/// 연결 복제(`clones`, 쓰는 VM 이름이나 붙지 않은 볼륨 이름)가 남은 템플릿을 VM으로 되돌릴 수 없다는 문구(계획 58
/// `template_in_use`)
pub(crate) fn template_in_use_message(template: &str, clones: &[&str]) -> String {
    format!(
        "template {template:?} has {} linked clone(s) ({}); delete or flatten them first",
        clones.len(),
        short_list(clones)
    )
}

/// 기반 볼륨 `bases`(id)의 연결 복제(`base`가 그 id인 볼륨)
pub(crate) fn linked_clones<'a>(volumes: &'a [Volume], bases: &[Uuid]) -> Vec<&'a Volume> {
    volumes
        .iter()
        .filter(|v| v.spec.base.is_some_and(|b| bases.contains(&b)))
        .collect()
}

/// 연결 복제 `clones`를 사람에게 보일 이름: 쓰는 VM의 이름, 붙지 않은 복제는 볼륨 이름(이름 순, 겹침 없음)
pub(crate) fn clone_names<'a>(clones: &[&'a Volume], vms: &'a [Vm]) -> Vec<&'a str> {
    let mut names: Vec<&str> = clones
        .iter()
        .map(|c| {
            vms.iter()
                .find(|vm| vm.spec.disks.iter().any(|d| d.volume == c.name))
                .map_or(c.name.as_str(), |vm| vm.name.as_str())
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// 볼륨 `volume`(이름)을 디스크로 가진 템플릿 VM
pub(crate) fn template_of<'a>(vms: &'a [Vm], volume: &str) -> Option<&'a Vm> {
    vms.iter()
        .find(|vm| vm.spec.template && vm.spec.disks.iter().any(|d| d.volume == volume))
}

/// 템플릿 디스크라 크기를 바꾸거나 지울 수 없다(409 `volume_is_template`, 계획 58 §3)
fn volume_is_template(volume: &str, template: &str) -> ApiError {
    ApiError::conflict(
        "volume_is_template",
        format!("volume {volume:?} is a disk of template {template:?}"),
    )
}

/// CD-ROM 드라이브에 꽂힌 ISO를 지울 수 없다는 문구. VM이 여럿이면 첫 VM과 나머지 수를 적는다.
pub(crate) fn in_cdrom_message(volume: &str, users: &[&Vm]) -> String {
    let mut message = format!(
        "volume {volume:?} is in the CD-ROM drive of VM {:?}",
        users[0].name
    );
    if users.len() > 1 {
        message.push_str(&format!(" and {} more", users.len() - 1));
    }
    message
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResizeRequest {
    /// 새 크기(바이트). 지금보다 크고 512의 배수여야 한다.
    pub size_bytes: u64,
}

/// 볼륨 크기를 늘릴 Task를 돌려준다(202). 줄이지는 않는다(400). VM이 쓰면 그 VM의 스냅샷
/// Task와 같은 노드(실행 중이면 그 VM의 노드, 꺼졌으면 다음에 돌 노드)의 실행기에서 차례로 돌도록
/// 거기서 한다. 쓰는 VM이 없거나 정할 노드가 없으면 볼륨의 노드에서 한다. 쓰는 VM이 켜지거나 꺼지는
/// 중이면(원하는 상태와 실제 상태가 다르다) 409 `vm_busy`다. 꺼진 VM으로 보고 저장소로 늘리는 동안
/// 시작한 QEMU가 늘리는 중인 볼륨을 열 수 있다. 템플릿의 디스크는 409 `volume_is_template`이다(배포 때 디스크 크기를
/// 준다).
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
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    if !volume.spec.content.is_disk() {
        return Err(ApiError::bad_request("ISO volumes cannot be resized"));
    }
    // 볼륨(붙은 VM 또는 풀)의 권한은 경로표가 봤다. 늘리는 것은 풀의 몫도 쓴다.
    state.authz.require(
        &principal,
        Privilege::StorageAllocate,
        &crate::authz::pool_target(&state.cluster, &volume.spec.pool)?,
    )?;
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
    if let Some(template) = template_of(&vms, &volume.name) {
        return Err(volume_is_template(&volume.name, &template.name));
    }
    let user = vms.iter().find(|vm| vm_uses_volume(vm, &volume.name));
    if let Some(vm) = user {
        // 떼는 중인 디스크는 스펙 순번이 없어 늘릴 수 없다.
        if !vm.spec.disks.iter().any(|d| d.volume == volume.name) {
            return Err(volume_in_use(vm, &volume.name, String::new()));
        }
        check_not_busy(&state, vm)?;
    }
    // 붙은 VM의 폴더 할당량(디스크)
    crate::quota::precheck(
        &state.cluster,
        crate::quota::Change::Volume {
            name: &volume.name,
            size: req.size_bytes,
        },
    )?;
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

/// 연결 복제 볼륨을 기반과 떼어 독립 볼륨으로 만들 Task(`volume-flatten`)를 돌려준다(202, 계획 58 §7). 볼륨을
/// 쓸 수 있는 노드에서 돈다(쓰는 VM이 있으면 그 VM의 디스크 노드). 모든 노드가 `templates`를 알아야 하고(409
/// `cluster_upgrading`), 연결 복제가 아니면 400, 아직 만드는 중이면 409 `volume_not_ready`, 쓰는 VM이 꺼져 있지 않으면
/// 409 `vm_running`(실행 중 평탄화는 없다), 그 VM의 스냅샷·복제·백업·마이그레이션 Task가 끝나지 않았으면 409
/// `vm_busy`다. 파일 풀(dir·NFS)의 디스크는 VM에 스냅샷이 있으면 409 `vm_has_snapshots`(qcow2 내부 스냅샷이 기반을
/// 잃는다). 기반이 쓰는 공간을 노드가 알린 풀 여유가 담지 못하면 409 `no_capacity`다.
#[utoipa::path(post, path = "/volumes/{key}/flatten", tag = "storage",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn flatten_volume(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, TEMPLATES)?;
    // 같은 VM을 켜는 요청과 엇갈리지 않게 확인과 Task 만들기를 VM 쓰기와 함께 잠근다.
    let _lock = state.vm_lock.lock().await;
    let volume = lookup::<VolumeSpec>(&state.cluster, &key)?;
    let Some(base) = volume.spec.base else {
        return Err(ApiError::bad_request(format!(
            "volume {:?} is not a linked clone",
            volume.name
        )));
    };
    if volume.status.phase != VolumePhase::Ready {
        return Err(ApiError::conflict(
            "volume_not_ready",
            format!("volume {:?} is still being created", volume.name),
        ));
    }
    let pool = find_pool(&state, &volume.spec.pool)?;
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let user = vms.iter().find(|vm| vm_uses_volume(vm, &volume.name));
    if let Some(vm) = user {
        if !is_stopped(vm) {
            return Err(ApiError::conflict(
                "vm_running",
                format!(
                    "cannot flatten disk {:?} of VM {:?} while it is running; stop it first",
                    volume.name, vm.name
                ),
            ));
        }
        check_not_busy(&state, vm)?;
        check_no_snapshot_task(&state, vm)?;
        let snapshots = count_snapshots(&state, vm.id)?;
        if block_pool_kind(&pool).is_none() && snapshots > 0 {
            return Err(ApiError::conflict(
                "vm_has_snapshots",
                format!(
                    "VM {:?} has {snapshots} snapshot(s); delete them before flattening disk {:?}",
                    vm.name, volume.name
                ),
            ));
        }
    }
    let node = match user.and_then(|vm| vm_placement(&state.cluster, Some(state.node), vm)) {
        Some(node) => node,
        None => volume_node(&state, &volume)?,
    };
    let base = state.cluster.get::<VolumeSpec>(base)?;
    check_flatten_space(&state, &pool, node, base.as_ref())?;
    let task = create_task(&state.cluster, TaskOp::VolumeFlatten, volume.id, node).await?;
    let base = base.map_or_else(|| "?".to_string(), |b| b.name);
    audit.detail(format!("volume flatten {} (base {base})", volume.name));
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 평탄화가 기반의 데이터(할당량, 모르면 보지 않는다)를 복제로 가져올 자리가 Task 노드가 알린 풀 여유(모르면 보지
/// 않는다)에 있는지. 아니면 409 `no_capacity`.
fn check_flatten_space(
    state: &AppState,
    pool: &StoragePool,
    node: Uuid,
    base: Option<&Volume>,
) -> Result<(), ApiError> {
    let Some(need) = base.and_then(|b| b.status.allocated_bytes) else {
        return Ok(());
    };
    let available = state
        .cluster
        .get::<NodeSpec>(node)?
        .and_then(|n| n.status.pools.get(&pool.name).cloned())
        .filter(|p| p.pool_id == pool.id && p.total_bytes > 0)
        .map(|p| p.available_bytes);
    match available {
        Some(available) if available < need => Err(ApiError::conflict(
            "no_capacity",
            format!(
                "storage pool {:?} has {available} bytes free, but flattening needs about {need} bytes",
                pool.name
            ),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_clone_messages_list_three_names() {
        assert_eq!(
            has_clones_message("tmpl-disk0", &["web-1-disk0", "web-2-disk0"]),
            "volume \"tmpl-disk0\" is the base of 2 linked clone(s) (web-1-disk0, web-2-disk0); delete or flatten them first"
        );
        assert_eq!(
            template_in_use_message("tmpl", &["web-1", "web-2", "web-3", "web-4"]),
            "template \"tmpl\" has 4 linked clone(s) (web-1, web-2, web-3, …); delete or flatten them first"
        );
    }

    #[test]
    fn redact_url_hides_user_info_only() {
        for (url, shown) in [
            ("https://a.example/x.iso", "https://a.example/x.iso"),
            (
                "https://user:pw@a.example/x.iso",
                "https://***@a.example/x.iso",
            ),
            ("http://user@a.example:8080", "http://***@a.example:8080"),
            // 경로의 `@`는 사용자 정보가 아니다. 쿼리 문자열은 통째로 가린다(서명·토큰).
            (
                "https://a.example/x@y.iso?u=a@b",
                "https://a.example/x@y.iso?***",
            ),
            (
                "https://u:p@a.example/x.iso?X-Amz-Signature=abc&token=t#f",
                "https://***@a.example/x.iso?***",
            ),
            ("https://a.example?sig=1", "https://a.example?***"),
            ("https://u:p@ss@a.example/", "https://***@a.example/"),
            ("not a url", "not a url"),
        ] {
            assert_eq!(redact_url(url), shown, "{url}");
        }
    }

    #[test]
    fn sha256_is_64_hex_digits_stored_lowercase() {
        let sum = "AB".repeat(32);
        assert_eq!(parse_hex("sha256", &sum, 64).unwrap(), "ab".repeat(32));
        assert!(parse_hex("sha256", &"ab".repeat(31), 64).is_err());
        assert!(parse_hex("sha256", &"zz".repeat(32), 64).is_err());
    }

    #[test]
    fn a_download_is_checked_one_way_at_most() {
        let sha512 = "CD".repeat(64);
        let check = DownloadCheck::parse(None, Some(&sha512), None).unwrap();
        assert_eq!(check.sha512, Some("cd".repeat(64)));
        assert_eq!(check.describe(), " (sha512 cdcd…)");
        let e = DownloadCheck::parse(Some(&"ab".repeat(32)), Some(&sha512), None).unwrap_err();
        assert!(e.message.contains("only one"), "{}", e.message);
        let e = DownloadCheck::parse(None, Some(&"ab".repeat(32)), None).unwrap_err();
        assert_eq!(e.message, "sha512 must be 128 hex digits");
        let e = DownloadCheck::parse(None, None, Some("ftp://a.example/SUMS")).unwrap_err();
        assert_eq!(e.message, "checksum_url must be an http or https URL");
        let check =
            DownloadCheck::parse(None, None, Some("https://u:p@a.example/SHA256SUMS")).unwrap();
        assert_eq!(
            check.describe(),
            " (checksum file https://***@a.example/SHA256SUMS)"
        );
        assert_eq!(
            DownloadCheck::parse(None, None, None).unwrap(),
            DownloadCheck::default()
        );
    }
}
