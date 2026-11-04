//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다. 실행과 지우기는 그 노드의
//! Task 실행기가 한다. ESXi·vCenter 데이터스토어의 VM은 `.vmx`·디스크립터를 HTTPS로 읽어([`esxi`]·
//! [`vmx`]) 같은 리소스를 만들고, 비밀번호는 요청을 받은 노드의 메모리([`ImportCredentials`])에만 둔다.

pub mod esxi;
pub mod ova;
pub mod ovf;
pub mod vmx;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{StreamExt, TryStreamExt};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::network::{NetworkSpec, USER_NETWORK};
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{
    CpuModel, CpuSpec, DiskBus, DiskSpec, MacAddr, MachineType, NicSpec, PowerState, RestartPolicy,
    VmSpec,
};
use opl_types::vm_import::{
    EsxiConnection, EsxiSource, ImportPhase, ImportProposal, ImportRun, ImportSource, OvaSource,
    SATA_PORTS, SourceDisk, SourceVm, VmImport, VmImportSpec, VmImportStatus,
    normalize_fingerprint, propose,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::{check_online, node_name};
use crate::error::{ApiError, Json, Query};
use crate::resources::{check_name, lookup};
use crate::state::AppState;
use crate::tasks::{create_task, volume_name};
use crate::volumes::{ImageFile, check_pool_on, check_upload_space, content_length, receive};
use esxi::EsxiError;
use ova::{OvaError, parse_ova, read_index};
use vmx::{MAX_TEXT_BYTES, VmxError};

/// `/vm-imports/ova`·`/vm-imports/esxi` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAMES: [&str; 2] = ["ova", "esxi"];
/// 이 노드 메모리에 자격 증명이 없는(opld가 다시 시작했거나 요청이 끊긴) ESXi 가져오기의 오류
pub(crate) const CREDENTIALS_LOST: &str = "the ESXi credentials are no longer on this node (opld restarted or the request was interrupted); create the import again";
/// browse가 `.vmx`를 동시에 읽는 개수
const BROWSE_CONCURRENCY: usize = 8;
/// browse가 보는 VM 디렉터리의 최대 개수(넘으면 자른다)
const BROWSE_MAX_DIRS: usize = 500;
/// 가져오기 디렉터리 안의 원본 OVA 파일 이름
pub(crate) const SOURCE_OVA: &str = "source.ova";
/// 가져오기 Task가 디스크를 하나씩 추출해 두는 파일 이름(가져오기 디렉터리 안)
pub(crate) const EXTRACTED_DISK: &str = "disk.vmdk";
/// ESXi 가져오기 Task가 디스크(extent)를 하나씩 받아 두는 파일 이름(가져오기 디렉터리 안)
pub(crate) const DOWNLOADED_DISK: &str = "disk.raw";

/// 가져오기 `id`의 원본 디렉터리
pub(crate) fn import_dir(upload_dir: &FsPath, id: Uuid) -> PathBuf {
    upload_dir.join("imports").join(id.to_string())
}

/// OVA를 쓸 수 없으면 400 `invalid_ova`(이유를 메시지에), 읽기 오류는 500
pub(crate) fn ova_error(e: OvaError) -> ApiError {
    match e {
        OvaError::Invalid(message) => {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_ova", message)
        }
        OvaError::Io(e) => ApiError::internal(e),
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaUploadQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올린 파일 이름(표시용). 없으면 `{name}.ova`
    pub file_name: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct OvaCheckQuery {
    /// 가져오기 이름
    pub name: String,
    /// 올릴 파일 크기(바이트)
    pub size: u64,
}

/// 이 노드에 새 가져오기를 만들 수 있는지 본다(권한, 이름, 위트니스 노드). OVA 올리기와 ESXi 가져오기가
/// 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면 저장소에 만들 때 `name_taken`이 난다.
fn check_new(state: &AppState, principal: &Principal, name: &str) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if RESERVED_NAMES.contains(&name) {
        return Err(ApiError::bad_request(format!(
            "the name {name} is reserved"
        )));
    }
    // 원본은 받은 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
    if let Some(node) = state.cluster.get::<NodeSpec>(state.node)?
        && node.spec.witness
    {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness node and cannot run VMs; upload to another node",
            node.name
        )));
    }
    if state.cluster.get_by_name::<VmImportSpec>(name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("vm-import {name:?} already exists"),
        ));
    }
    Ok(())
}

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    check_new(state, principal, name)?;
    check_upload_space(state, size).await
}

/// OVA를 받기 전에 받을 수 있는지 확인한다(204). 거절 이유는 올리기와 같다. 본문은 받지 않는다.
#[utoipa::path(post, path = "/vm-imports/ova/check", tag = "vm-imports",
    params(OvaCheckQuery),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn check_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaCheckQuery>,
) -> Result<StatusCode, ApiError> {
    check_upload(&state, &principal, &q.name, q.size).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 요청 본문(OVA)을 이 노드에 받아 해석하고, 원본 VM과 제안을 담은 `ready` 가져오기를 돌려준다(201).
/// OVA를 쓸 수 없으면 400 `invalid_ova`. 실패하거나 연결이 끊기면 받은 파일을 지운다.
#[utoipa::path(post, path = "/vm-imports/ova", tag = "vm-imports",
    params(OvaUploadQuery),
    request_body(content = ImageFile, content_type = "application/octet-stream"),
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn upload_ova(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<OvaUploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    let length = content_length(&headers)?;
    check_upload(&state, &principal, &q.name, length).await?;
    let file_name = q.file_name.unwrap_or_else(|| format!("{}.ova", q.name));
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Ova(OvaSource {
            file_name,
            size_bytes: length,
        }),
    };
    let mut import: VmImport = Resource::new(q.name, spec);
    let dir = import_dir(&state.upload_dir, import.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(ApiError::internal)?;
    // 여기서부터 실패하거나 클라이언트가 끊어 이 future가 버려지면 디렉터리를 지운다.
    let mut guard = DirGuard {
        dir: dir.clone(),
        armed: true,
    };
    let source = dir.join(SOURCE_OVA);
    receive(&source, body, length).await?;
    let ova = tokio::task::spawn_blocking(move || parse_ova(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(ova_error)?;
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&ova.vm, &networks);
    let mut warnings = ova.warnings;
    warnings.extend(proposal_warnings);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(ova.vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 올리기를 끝내지 못한 가져오기 디렉터리를 지운다. 정상 경로에서는 `armed`를 내린다.
struct DirGuard {
    dir: PathBuf,
    armed: bool,
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let dir = self.dir.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
                tracing::warn!(path = %dir.display(), error = %e, "cannot remove a failed upload");
            }
        });
    }
}

/// ESXi 가져오기의 자격 증명(가져오기 id → 연결). 비밀번호를 Raft·Task·로그에 남기지 않으려고 요청을
/// 받은 노드의 메모리에만 둔다. opld가 다시 시작하면 사라진다(그 가져오기는 시작 정리가 `failed`로 바꾼다).
#[derive(Default)]
pub struct ImportCredentials(Mutex<HashMap<Uuid, EsxiConnection>>);

impl ImportCredentials {
    pub fn insert(&self, import: Uuid, conn: EsxiConnection) {
        self.0.lock().unwrap().insert(import, conn);
    }

    pub fn get(&self, import: Uuid) -> Option<EsxiConnection> {
        self.0.lock().unwrap().get(&import).cloned()
    }

    pub fn remove(&self, import: Uuid) {
        self.0.lock().unwrap().remove(&import);
    }

    pub fn is_empty(&self) -> bool {
        self.0.lock().unwrap().is_empty()
    }
}

/// 만들기를 끝내지 못한 가져오기의 자격 증명을 지운다. 정상 경로에서는 `armed`를 내린다.
struct CredentialGuard {
    credentials: Arc<ImportCredentials>,
    import: Uuid,
    armed: bool,
}

impl CredentialGuard {
    fn insert(credentials: Arc<ImportCredentials>, import: Uuid, conn: EsxiConnection) -> Self {
        credentials.insert(import, conn);
        CredentialGuard {
            credentials,
            import,
            armed: true,
        }
    }
}

impl Drop for CredentialGuard {
    fn drop(&mut self) {
        if self.armed {
            self.credentials.remove(self.import);
        }
    }
}

/// ESXi 오류를 API 오류로. 지문이 없으면 409 `untrusted_certificate`, 다르면 409 `fingerprint_mismatch`
/// (둘 다 `details.fingerprint`에 서버 인증서의 지문), 인증 실패 400 `esxi_auth`, 연결할 수 없으면 502
/// `esxi_unreachable`. 없는 파일·그 밖의 HTTP 응답·잘못된 응답은 400(메시지에 이유).
pub(crate) fn esxi_error(e: EsxiError) -> ApiError {
    let with_fingerprint = |mut error: ApiError, fingerprint: &str| {
        error.details = Some(serde_json::json!({ "fingerprint": fingerprint }));
        error
    };
    match e {
        EsxiError::Untrusted(actual) => with_fingerprint(
            ApiError::conflict(
                "untrusted_certificate",
                "verify the ESXi certificate fingerprint",
            ),
            &actual,
        ),
        EsxiError::Fingerprint { ref actual, .. } => with_fingerprint(
            ApiError::conflict("fingerprint_mismatch", e.to_string()),
            actual,
        ),
        EsxiError::Auth => ApiError::new(StatusCode::BAD_REQUEST, "esxi_auth", e.to_string()),
        EsxiError::Connect(_) => {
            ApiError::new(StatusCode::BAD_GATEWAY, "esxi_unreachable", e.to_string())
        }
        EsxiError::NotFound(_)
        | EsxiError::Http(_)
        | EsxiError::TooLarge { .. }
        | EsxiError::Invalid(_) => ApiError::bad_request(e.to_string()),
        EsxiError::Io(e) => ApiError::internal(e),
    }
}

/// 요청의 지문을 정규화한다. 모양이 틀리면 연결하기 전에 400(어떤 인증서와도 맞지 않으므로).
fn check_fingerprint(conn: &mut EsxiConnection) -> Result<(), ApiError> {
    if let Some(f) = &conn.fingerprint {
        let normalized = normalize_fingerprint(f).ok_or_else(|| {
            ApiError::bad_request(format!(
                "invalid fingerprint {f:?}; expected the 64 hex digits of a SHA-256 fingerprint"
            ))
        })?;
        conn.fingerprint = Some(normalized);
    }
    Ok(())
}

/// 켜진 VM이 디렉터리에 남기는 항목인지: 잠금(`.lck`로 끝남, 디렉터리일 수도), NFS 잠금(`.lck-`로
/// 시작), 스왑 파일(`.vswp`, 켜진 동안만 있다). 짐작일 뿐이다.
fn lock_file(name: &str) -> bool {
    name.ends_with(".lck") || name.starts_with(".lck-") || name.ends_with(".vswp")
}

/// 일시 중단된 VM이 디렉터리에 남기는 메모리 상태 파일(`.vmss`)인지
fn suspend_file(name: &str) -> bool {
    name.ends_with(".vmss")
}

fn vmx_error(e: VmxError) -> ApiError {
    ApiError::bad_request(e.0)
}

/// 연결 자체의 문제(인증·지문·연결)인지. browse는 이것만 요청 전체의 실패로 보고, 디렉터리 하나를 읽지
/// 못한 것은 그 디렉터리만 건너뛴다.
fn connection_failed(e: &EsxiError) -> bool {
    matches!(
        e,
        EsxiError::Auth
            | EsxiError::Connect(_)
            | EsxiError::Untrusted(_)
            | EsxiError::Fingerprint { .. }
            | EsxiError::Io(_)
    )
}

/// browse 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiBrowse {
    pub datastores: Vec<EsxiDatastore>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiDatastore {
    pub name: String,
    /// 맨 위 디렉터리마다 첫 `.vmx`(경로 순서)
    pub vms: Vec<EsxiVm>,
}

/// 데이터스토어의 VM 요약. `.vmx`를 해석할 수 없으면 `error`가 있고 나머지는 빈 값이다.
#[derive(Debug, Serialize, ToSchema)]
pub struct EsxiVm {
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
    pub name: String,
    pub guest_os: String,
    pub vcpus: u32,
    pub memory_mib: u64,
    pub disks: u32,
    /// 디렉터리에 잠금·스왑 파일(`.lck`, `.lck-…`, `.vswp`)이 있거나(켜져 있을 수 있다) 일시 중단 파일
    /// (`.vmss`)이 있다
    pub locked: bool,
    pub error: Option<String>,
}

/// ESXi·vCenter에 연결해 데이터스토어마다 맨 위 디렉터리의 VM(`.vmx`)을 요약한다. 지문이 없으면 409
/// `untrusted_certificate`(`details.fingerprint`), 다르면 409 `fingerprint_mismatch`, 인증 실패 400
/// `esxi_auth`, 연결할 수 없으면 502 `esxi_unreachable`. VM 디렉터리는 500개까지 본다.
#[utoipa::path(post, path = "/vm-imports/esxi/browse", tag = "vm-imports",
    request_body = EsxiConnection,
    responses((status = 200, body = EsxiBrowse), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn browse_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(mut conn): Json<EsxiConnection>,
) -> Result<Json<EsxiBrowse>, ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    check_fingerprint(&mut conn)?;
    let datastores = esxi::list(&conn, None, "").await.map_err(esxi_error)?;
    let mut dirs = Vec::new();
    for ds in &datastores {
        match esxi::list(&conn, Some(&ds.name), "").await {
            Ok(entries) => dirs.extend(
                entries
                    .into_iter()
                    .filter(|e| e.is_dir)
                    .map(|e| (ds.name.clone(), e.name)),
            ),
            Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
            Err(e) => {
                tracing::info!(datastore = %ds.name, error = %e, "cannot list an ESXi datastore")
            }
        }
    }
    dirs.truncate(BROWSE_MAX_DIRS);
    let conn = &conn;
    let vms: Vec<Option<(String, EsxiVm)>> = futures_util::stream::iter(dirs)
        .map(|(ds, dir)| async move {
            Ok::<_, ApiError>(browse_dir(conn, &ds, &dir).await?.map(|vm| (ds, vm)))
        })
        .buffered(BROWSE_CONCURRENCY)
        .try_collect()
        .await?;
    let mut vms: Vec<(String, EsxiVm)> = vms.into_iter().flatten().collect();
    let datastores = datastores
        .iter()
        .map(|ds| EsxiDatastore {
            name: ds.name.clone(),
            vms: vms
                .extract_if(.., |(name, _)| *name == ds.name)
                .map(|(_, vm)| vm)
                .collect(),
        })
        .collect();
    Ok(Json(EsxiBrowse { datastores }))
}

/// 디렉터리 `dir`의 첫 `.vmx`를 요약한다. `.vmx`가 없거나 디렉터리를 읽을 수 없으면 None.
async fn browse_dir(
    conn: &EsxiConnection,
    datastore: &str,
    dir: &str,
) -> Result<Option<EsxiVm>, ApiError> {
    let entries = match esxi::list(conn, Some(datastore), dir).await {
        Ok(entries) => entries,
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => {
            tracing::info!(datastore, dir, error = %e, "cannot list an ESXi directory");
            return Ok(None);
        }
    };
    let Some(file) = entries
        .iter()
        .find(|e| !e.is_dir && e.name.to_ascii_lowercase().ends_with(".vmx"))
    else {
        return Ok(None);
    };
    let path = format!("{dir}/{}", file.name);
    let locked = entries
        .iter()
        .any(|e| lock_file(&e.name) || suspend_file(&e.name));
    let summary = match esxi::get_small(conn, datastore, &path, MAX_TEXT_BYTES).await {
        Ok(text) => vmx::parse_vmx(&text)
            .and_then(|v| vmx::vmx_to_source(&v, &path))
            .map_err(|e| e.0),
        Err(e) if connection_failed(&e) => return Err(esxi_error(e)),
        Err(e) => Err(e.to_string()),
    };
    let vm = match summary {
        Ok((vm, _, disks)) => EsxiVm {
            vmx: path,
            name: vm.name,
            guest_os: vm.guest_os,
            vcpus: vm.vcpus,
            memory_mib: vm.memory_mib,
            disks: disks.len() as u32,
            locked,
            error: None,
        },
        Err(error) => EsxiVm {
            name: file
                .name
                .get(..file.name.len() - ".vmx".len())
                .unwrap_or(&file.name)
                .to_string(),
            vmx: path,
            guest_os: String::new(),
            vcpus: 0,
            memory_mib: 0,
            disks: 0,
            locked,
            error: Some(error),
        },
    };
    Ok(Some(vm))
}

/// ESXi에서 가져오기 만들기 요청
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EsxiImportRequest {
    /// 가져오기 이름
    pub name: String,
    /// 연결(`fingerprint`가 있어야 한다)
    pub connection: EsxiConnection,
    pub datastore: String,
    /// 데이터스토어 안의 `.vmx` 경로(`web01/web01.vmx`)
    pub vmx: String,
}

/// 데이터스토어 안의 상대 경로이고 `.vmx`로 끝나는지
fn valid_vmx_path(path: &str) -> bool {
    path.ends_with(".vmx")
        && !path.contains('\\')
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}

/// ESXi·vCenter 데이터스토어의 VM으로 `ready` 가져오기를 만든다(201). 이 노드가 `.vmx`와 디스크
/// 디스크립터를 읽어 원본 VM과 제안을 만들고, 자격 증명은 이 노드의 메모리에만 둔다(실행 Task도 이
/// 노드에서 돈다). 연결 오류는 browse와 같다(지문이 없으면 409 `untrusted_certificate`). 스냅샷이 있는
/// VM, 다른 데이터스토어의 디스크, 통째로 받을 수 없는 디스크는 400. 잠금·일시 중단 파일이 있으면 경고만
/// 한다.
#[utoipa::path(post, path = "/vm-imports/esxi", tag = "vm-imports",
    request_body = EsxiImportRequest,
    responses((status = 201, body = Resource<VmImportSpec, VmImportStatus>), (status = "4XX", body = crate::error::ErrorBody), (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_esxi(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<EsxiImportRequest>,
) -> Result<(StatusCode, Json<VmImport>), ApiError> {
    check_new(&state, &principal, &req.name)?;
    let EsxiImportRequest {
        name,
        connection: mut conn,
        datastore,
        vmx: vmx_path,
    } = req;
    check_fingerprint(&mut conn)?;
    if datastore.is_empty() || !valid_vmx_path(&vmx_path) {
        return Err(ApiError::bad_request(format!(
            "invalid .vmx path {vmx_path:?} on datastore {datastore:?}"
        )));
    }
    let text = esxi::get_small(&conn, &datastore, &vmx_path, MAX_TEXT_BYTES)
        .await
        .map_err(esxi_error)?;
    // 지문이 맞아 읽었으므로 지문은 있고 모양도 맞다.
    let fingerprint = conn
        .fingerprint
        .as_deref()
        .and_then(normalize_fingerprint)
        .ok_or_else(|| ApiError::bad_request("invalid certificate fingerprint"))?;
    conn.fingerprint = Some(fingerprint.clone());
    let parsed = vmx::parse_vmx(&text).map_err(vmx_error)?;
    let (mut vm, mut warnings, disks) =
        vmx::vmx_to_source(&parsed, &vmx_path).map_err(vmx_error)?;
    for disk in disks {
        let path = vmx::disk_path(&vmx_path, &datastore, &disk.path).map_err(vmx_error)?;
        let shown = format!("[{datastore}] {path}");
        let text = match esxi::get_small(&conn, &datastore, &path, MAX_TEXT_BYTES).await {
            Ok(text) => text,
            // 크기 제한을 넘었다: 디스크립터가 따로 없는 VMDK(스파스·호스트형)다.
            Err(EsxiError::TooLarge { .. }) => {
                return Err(ApiError::bad_request(format!(
                    "{shown} is not a VMDK descriptor; only VMFS and flat disks can be imported"
                )));
            }
            Err(e) => return Err(esxi_error(e)),
        };
        let (file, capacity_bytes) = vmx::parse_descriptor(&text)
            .and_then(|d| vmx::check_flat(&d))
            .map_err(|e| ApiError::bad_request(format!("{shown}: {e}")))?;
        // extent는 디스크립터와 같은 디렉터리에 있다.
        let file = match path.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{file}"),
            None => file,
        };
        vm.disks.push(SourceDisk {
            name: disk.name,
            file,
            capacity_bytes,
            controller: disk.controller,
        });
    }
    let dir = vmx_path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let entries = esxi::list(&conn, Some(&datastore), dir)
        .await
        .map_err(esxi_error)?;
    if entries.iter().any(|e| suspend_file(&e.name)) {
        warnings.insert(
            0,
            "the VM is suspended; power it off in VMware before importing (its memory state is not imported)".into(),
        );
    }
    if entries.iter().any(|e| lock_file(&e.name)) {
        warnings.insert(
            0,
            "the VM looks powered on (lock files found); power it off before importing".into(),
        );
    }
    let (_, networks) = state.cluster.list::<NetworkSpec>()?;
    let networks: Vec<String> = networks.into_iter().map(|n| n.name).collect();
    let (proposal, proposal_warnings) = propose(&vm, &networks);
    warnings.extend(proposal_warnings);
    let spec = VmImportSpec {
        node: state.node,
        source: ImportSource::Esxi(EsxiSource {
            host: conn.host.clone(),
            port: conn.port,
            datacenter: conn.datacenter.clone(),
            datastore,
            vmx: vmx_path,
            user: conn.user.clone(),
            fingerprint,
        }),
    };
    let mut import: VmImport = Resource::new(name, spec);
    import.status = VmImportStatus {
        phase: ImportPhase::Ready,
        source_vm: Some(vm),
        proposal: Some(proposal),
        warnings,
        vm: None,
        error: None,
    };
    // 만들자마자 다른 노드로 온 실행 요청의 Task가 자격 증명을 찾을 수 있게 먼저 넣는다. 만들기가
    // 실패하거나 클라이언트가 끊어 이 future가 버려지면 지운다.
    let mut guard = CredentialGuard::insert(state.import_credentials.clone(), import.id, conn);
    import.revision = state.cluster.create(&import).await?;
    guard.armed = false;
    Ok((StatusCode::CREATED, Json(import)))
}

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` OVA 가져오기인데 원본이 없으면 `failed`로 바꾼다.
/// 이 노드의 `ready` ESXi 가져오기는 메모리의 자격 증명을 잃었으므로 `failed`로 바꾼다(ESXi 가져오기는
/// 실행할 때만 디렉터리를 쓰므로 디렉터리는 보지 않는다).
/// 실행할 Task가 없는 이 노드의 `importing` 가져오기(Task를 만들기 전에 API가 죽었다)는 `ready`로
/// 되돌리고 이유를 남긴다. 다만 그 가져오기의 Task가 VM을 만든 뒤 죽었으면(대상 VM이 있다) `done`으로
/// 끝내고 원본 디렉터리를 지운다. 끝나지 않은 Task가 있으면 실행기가 그 Task를 실패로 끝내며 정리한다.
/// `tasks`는 `revision`에서 읽은 Task 목록이다. 그보다 나중에 바뀐 가져오기는 그 목록으로 판단할 수 없다.
pub(crate) async fn sweep(
    cluster: &Cluster,
    node: Uuid,
    upload_dir: &FsPath,
    revision: u64,
    tasks: &[Task],
) {
    let imports = match cluster.list::<VmImportSpec>() {
        Ok((_, imports)) => imports,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list vm imports");
            return;
        }
    };
    if let Ok(entries) = std::fs::read_dir(upload_dir.join("imports")) {
        for entry in entries.flatten() {
            let known = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::try_parse(name).ok())
                .is_some_and(|id| imports.iter().any(|i| i.id == id));
            if !known {
                let path = entry.path();
                tracing::warn!(path = %path.display(), "removing import files without an import");
                let removed = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                if let Err(e) = removed {
                    tracing::warn!(path = %path.display(), error = %e, "cannot remove import files");
                }
            }
        }
    }
    for import in imports {
        if import.spec.node != node {
            continue;
        }
        let mut status = import.status.clone();
        let running = tasks.iter().any(|t| {
            !t.status.state.is_finished()
                && matches!(t.spec.op, TaskOp::VmImport { import: i, .. } if i == import.id)
        });
        if status.phase == ImportPhase::Importing && import.revision <= revision && !running {
            match made_vm(cluster, import.id, tasks) {
                Some(vm) => {
                    tracing::warn!(import = %import.name, "the import made its VM before it was interrupted");
                    status.phase = ImportPhase::Done;
                    status.vm = Some(vm);
                    status.error = None;
                    let dir = import_dir(upload_dir, import.id);
                    if let Err(e) = std::fs::remove_dir_all(&dir)
                        && e.kind() != std::io::ErrorKind::NotFound
                    {
                        tracing::warn!(path = %dir.display(), error = %e, "cannot remove import files");
                    }
                }
                None => {
                    tracing::warn!(import = %import.name, "the import was interrupted");
                    status.phase = ImportPhase::Ready;
                    status.error = Some("the import was interrupted".into());
                }
            }
        }
        let esxi = matches!(import.spec.source, ImportSource::Esxi(_));
        if status.phase == ImportPhase::Ready && esxi {
            tracing::warn!(import = %import.name, "the ESXi credentials of the import were lost");
            status.phase = ImportPhase::Failed;
            status.error = Some(CREDENTIALS_LOST.into());
        } else if status.phase == ImportPhase::Ready
            && !import_dir(upload_dir, import.id).join(SOURCE_OVA).exists()
        {
            tracing::warn!(import = %import.name, "the source files of the import are gone");
            status.phase = ImportPhase::Failed;
            status.error = Some("source files are gone".into());
        }
        if status == import.status {
            continue;
        }
        if let Err(e) = cluster
            .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
            .await
        {
            tracing::warn!(import = %import.name, error = %e, "cannot record the import state");
        }
    }
}

/// 가져오기 `import`의 Task 가운데 대상 VM(미리 정한 새 VM id)이 이미 있는 것의 VM id. VM을 만든 뒤
/// 가져오기를 `done`으로 바꾸기 전에 opld가 죽었을 때 쓴다.
fn made_vm(cluster: &Cluster, import: Uuid, tasks: &[Task]) -> Option<Uuid> {
    tasks
        .iter()
        .filter(|t| matches!(t.spec.op, TaskOp::VmImport { import: i, .. } if i == import))
        .map(|t| t.spec.target)
        .find(|vm| matches!(cluster.get::<VmSpec>(*vm), Ok(Some(_))))
}

/// 원본 VM `src`를 실행 요청 `run`의 값(없으면 제안 `proposal`의 값)으로 옮긴 VM 스펙. `volumes`는 원본
/// 디스크 순서의 볼륨 이름이다. 첫 디스크로 켜고, 노드 없이 꺼진 VM이다. `keep_macs`면 원본 MAC을 쓰고,
/// 아니면 비워 두어 만들 때 새로 받는다. 원본 NIC의 네트워크가 `run.networks`에 없으면 제안의 같은 이름
/// 매핑을 쓰고, 그것도 없으면 오류다. SATA 디스크는 [`SATA_PORTS`]개까지다. 실행 API의 검사와 가져오기
/// Task가 같은 스펙을 만든다.
pub(crate) fn import_spec(
    src: &SourceVm,
    proposal: &ImportProposal,
    run: &ImportRun,
    volumes: &[String],
) -> Result<VmSpec, String> {
    let bus = run.disk_bus.unwrap_or(proposal.disk_bus);
    let model = run.nic_model.unwrap_or(proposal.nic_model);
    if bus == DiskBus::Sata && volumes.len() > SATA_PORTS {
        return Err(format!(
            "{} disks do not fit on SATA ({SATA_PORTS} ports); use virtio-blk",
            volumes.len()
        ));
    }
    let disks = volumes
        .iter()
        .enumerate()
        .map(|(i, volume)| DiskSpec {
            volume: volume.clone(),
            bus,
            boot_order: (i == 0).then_some(1),
        })
        .collect();
    let nics = src
        .nics
        .iter()
        .map(|nic| {
            let network = run
                .networks
                .get(&nic.network)
                .or_else(|| proposal.networks.get(&nic.network).and_then(Option::as_ref))
                .ok_or_else(|| format!("no network mapping for {:?}", nic.network))?;
            Ok(NicSpec {
                network: network.clone(),
                model,
                mac: nic.mac.filter(|_| run.keep_macs),
                security_groups: vec![],
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(VmSpec {
        cpu: CpuSpec {
            sockets: run.sockets.unwrap_or(proposal.sockets),
            cores: run.cores.unwrap_or(proposal.cores),
            model: CpuModel::Host,
        },
        memory_mib: run.memory_mib.unwrap_or(proposal.memory_mib),
        firmware: run.firmware.unwrap_or(proposal.firmware),
        machine: MachineType::Q35,
        disks,
        nics,
        power: PowerState::Stopped,
        restart: RestartPolicy::Never,
        node: None,
        snapshot_schedule: None,
        ha: false,
    })
}

/// 가져오기 `id`를 다루는 끝나지 않은 Task가 하는 일(실행, 지우기)
fn import_tasks(state: &AppState, id: Uuid) -> Result<Vec<&'static str>, ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    Ok(tasks
        .iter()
        .filter(|t| !t.status.state.is_finished())
        .filter_map(|t| match t.spec.op {
            TaskOp::VmImport { import, .. } if import == id => Some("being imported"),
            TaskOp::VmImportDelete if t.spec.target == id => Some("being deleted"),
            _ => None,
        })
        .collect())
}

fn import_busy(import: &VmImport, doing: &str) -> ApiError {
    ApiError::conflict(
        "import_busy",
        format!(
            "vm-import {:?} is {doing}; try again when the task has finished",
            import.name
        ),
    )
}

/// 가져오기를 다루는 Task가 끝나지 않았으면 409 `import_busy`. `importing`이어도 끝나지 않은 Task가
/// 없으면(원본 노드가 없어져 Task가 실패로 끝났다) 쉬는 것으로 본다.
fn check_idle(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    match import_tasks(state, import.id)?.first() {
        Some(doing) => Err(import_busy(import, doing)),
        None => Ok(()),
    }
}

/// 끝난 실행 Task가 대상 VM을 이미 만들었으면(VM을 만든 뒤 가져오기를 `done`으로 바꾸지 못했다) 가져오기를
/// `done`으로 끝내고 409 `import_done`. 원본이 이 노드에 있으면 원본 디렉터리와 ESXi 자격 증명도 지운다
/// (다른 노드의 것은 가져오기를 지울 때 그 노드가 지운다).
async fn finish_made(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    let Some(vm) = made_vm(&state.cluster, import.id, &tasks) else {
        return Ok(());
    };
    tracing::warn!(import = %import.name, "the import made its VM before it was interrupted");
    let status = VmImportStatus {
        phase: ImportPhase::Done,
        vm: Some(vm),
        error: None,
        ..import.status.clone()
    };
    match state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
            return Err(ApiError::conflict(
                "import_busy",
                format!(
                    "vm-import {:?} was changed by another request; try again",
                    import.name
                ),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    if import.spec.node == state.node {
        state.import_credentials.remove(import.id);
        let dir = import_dir(&state.upload_dir, import.id);
        if let Err(e) = tokio::fs::remove_dir_all(&dir).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %dir.display(), error = %e, "cannot remove import files");
        }
    }
    Err(ApiError::conflict(
        "import_done",
        format!("vm-import {:?} has already been imported", import.name),
    ))
}

/// 원본 디렉터리에 가장 큰 디스크를 추출하고도 여유가 남는지 본다(아니면 507). OVA 원본이 없으면 409
/// `import_failed`. ESXi 원본은 디스크를 통째로 받으므로(씬 디스크도) 가장 큰 디스크의 용량을 본다.
async fn check_extract_space(
    state: &AppState,
    import: &VmImport,
    src: &SourceVm,
) -> Result<(), ApiError> {
    if let ImportSource::Esxi(_) = import.spec.source {
        let largest = src.disks.iter().map(|d| d.capacity_bytes).max();
        return check_upload_space(state, largest.unwrap_or(0)).await;
    }
    let source = import_dir(&state.upload_dir, import.id).join(SOURCE_OVA);
    if !tokio::fs::try_exists(&source).await.unwrap_or(false) {
        return Err(ApiError::conflict(
            "import_failed",
            format!("the source files of vm-import {:?} are gone", import.name),
        ));
    }
    let index = tokio::task::spawn_blocking(move || read_index(&source))
        .await
        .map_err(ApiError::internal)?
        .map_err(ova_error)?;
    let largest = src
        .disks
        .iter()
        .filter_map(|d| index.get(&d.file))
        .map(|e| e.size)
        .max()
        .unwrap_or(0);
    check_upload_space(state, largest).await
}

/// 이 노드의 ESXi 가져오기인데 메모리에 자격 증명이 없으면(opld가 다시 시작했다) 가져오기를 `failed`로
/// 바꾸고 409 `import_failed`. 다시 실행해도 소용없으므로 Task를 만들지 않는다. 다른 노드의 것은 그
/// 노드의 Task가 본다.
async fn check_credentials(state: &AppState, import: &VmImport) -> Result<(), ApiError> {
    if !matches!(import.spec.source, ImportSource::Esxi(_))
        || import.spec.node != state.node
        || state.import_credentials.get(import.id).is_some()
    {
        return Ok(());
    }
    let status = VmImportStatus {
        phase: ImportPhase::Failed,
        error: Some(CREDENTIALS_LOST.into()),
        ..import.status.clone()
    };
    if let Err(e) = state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        tracing::warn!(import = %import.name, error = %e, "cannot record the import state");
    }
    Err(ApiError::conflict(
        "import_failed",
        format!(
            "vm-import {:?} cannot be run: {CREDENTIALS_LOST}",
            import.name
        ),
    ))
}

/// `ready` 가져오기를 `run`의 값으로 실행할 Task를 돌려준다(202). Task는 원본이 있는 노드에서 돌고, 대상은
/// 미리 정한 새 VM id다. 실행 Task가 끝나지 않았으면 409 `import_busy`, 이미 가져왔으면 `import_done`(끝난 실행
/// Task가 VM을 만들었으면 가져오기를 `done`으로 끝내고), 원본을 쓸 수
/// 없으면 `import_failed`, 원본 노드가 오프라인이면 `node_offline`. 원본 NIC의 네트워크마다 있는 opLite
/// 네트워크로 매핑해야 하고(400), VM 이름과 볼륨 이름 `{name}-disk{i}`가 비어 있어야 한다(409
/// `name_taken`). 풀은 원본 노드에서 쓸 수 있어야 하고(409 `pool_not_on_node`·`pool_unavailable`), 결과
/// VM 스펙은 VM 만들기의 검증을 통과해야 한다(400). 원본 MAC을 쓰면 다른 VM과 겹치지 않아야 한다(409
/// `mac_in_use`). 원본이 이 노드에 있으면 가장 큰 디스크를 추출하거나 받을 여유도 본다(507).
#[utoipa::path(post, path = "/vm-imports/{key}/run", tag = "vm-imports",
    params(("key" = String, Path, description = "id or name")),
    request_body = ImportRun,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn run_import(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Json(run): Json<ImportRun>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    state.authorize(&principal, Action::Write, VmSpec::KIND)?;
    state.authorize(&principal, Action::Write, VolumeSpec::KIND)?;
    let import = lookup::<VmImportSpec>(&state.cluster, &key)?;
    match import.status.phase {
        ImportPhase::Ready | ImportPhase::Importing => {
            check_idle(&state, &import)?;
            finish_made(&state, &import).await?;
        }
        ImportPhase::Done => {
            return Err(ApiError::conflict(
                "import_done",
                format!("vm-import {:?} has already been imported", import.name),
            ));
        }
        ImportPhase::Failed => {
            return Err(ApiError::conflict(
                "import_failed",
                format!(
                    "vm-import {:?} cannot be run: {}",
                    import.name,
                    import.status.error.as_deref().unwrap_or("it failed")
                ),
            ));
        }
    }
    check_online(&state, import.spec.node)?;
    check_credentials(&state, &import).await?;
    let (Some(src), Some(proposal)) = (&import.status.source_vm, &import.status.proposal) else {
        return Err(ApiError::conflict(
            "import_failed",
            format!("vm-import {:?} has no source VM", import.name),
        ));
    };
    check_name(&run.name)?;
    // 이름이 그새 쓰이면 Task가 VM이나 볼륨을 만들 때 실패한다.
    if state.cluster.get_by_name::<VmSpec>(&run.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", run.name),
        ));
    }
    let volumes: Vec<String> = src
        .disks
        .iter()
        .map(|d| volume_name(&run.name, &d.name, |_| false))
        .collect();
    for volume in &volumes {
        if state.cluster.get_by_name::<VolumeSpec>(volume)?.is_some() {
            return Err(ApiError::conflict(
                "name_taken",
                format!("volume {volume:?} already exists"),
            ));
        }
    }
    let spec = import_spec(src, proposal, &run, &volumes).map_err(ApiError::bad_request)?;
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    for nic in spec.nics.iter().filter(|n| n.network != USER_NETWORK) {
        if state
            .cluster
            .get_by_name::<NetworkSpec>(&nic.network)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "network {:?} not found",
                nic.network
            )));
        }
    }
    let pool = state
        .cluster
        .get_by_name::<StoragePoolSpec>(&run.pool)?
        .ok_or_else(|| ApiError::bad_request(format!("storage pool {:?} not found", run.pool)))?;
    check_pool_on(&state, &pool, import.spec.node, || {
        format!(
            "storage pool {:?} is not available on node {:?} holding vm-import {:?}",
            pool.name,
            node_name(&state, import.spec.node).unwrap_or_else(|_| import.spec.node.to_string()),
            import.name
        )
    })?;
    if import.spec.node == state.node {
        check_extract_space(&state, &import, src).await?;
    }
    // 원본 MAC이 그새 쓰이면 Task가 VM을 만들 때 실패한다.
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let used: BTreeSet<MacAddr> = vms
        .iter()
        .flat_map(|v| v.spec.nics.iter().filter_map(|n| n.mac))
        .collect();
    if let Some(mac) = spec
        .nics
        .iter()
        .filter_map(|n| n.mac)
        .find(|m| used.contains(m))
    {
        return Err(ApiError::conflict(
            "mac_in_use",
            format!("MAC address {mac} is already in use"),
        ));
    }

    let status = VmImportStatus {
        phase: ImportPhase::Importing,
        error: None,
        ..import.status.clone()
    };
    match state
        .cluster
        .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
            return Err(ApiError::conflict(
                "import_busy",
                format!(
                    "vm-import {:?} was changed by another request; try again",
                    import.name
                ),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    let op = TaskOp::VmImport {
        import: import.id,
        run,
    };
    match create_task(&state.cluster, op, Uuid::now_v7(), import.spec.node).await {
        Ok(task) => Ok((StatusCode::ACCEPTED, Json(task))),
        Err(e) => {
            // 실행할 Task가 없으면 다시 실행할 수 있게 되돌린다.
            if let Err(d) = state
                .cluster
                .update_status::<VmImportSpec>(import.id, &import.status)
                .await
            {
                tracing::warn!(import = %import.name, error = %d, "cannot return the import to ready");
            }
            Err(e)
        }
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct DeleteImportQuery {
    /// 원본 노드가 오프라인이거나 클러스터에 없어도 리소스만 지운다. 원본 파일은 그 노드에 남는다.
    #[serde(default)]
    pub force: bool,
}

/// 리소스만 지운 가져오기 지우기(`force`)의 응답
#[derive(Debug, Serialize, ToSchema)]
pub struct VmImportForgotten {
    pub message: String,
}

/// 원본 디렉터리와 리소스를 지울 Task를 돌려준다(202). Task는 원본 노드에서 돈다. 그것을 다루는 Task가
/// 끝나지 않았으면(`force`여도) 409 `import_busy`. 원본 노드가 오프라인이거나 클러스터에 없으면 409
/// `node_offline`이고, `force`면 리소스만 바로 지우고 200이다.
#[utoipa::path(delete, path = "/vm-imports/{key}", tag = "vm-imports",
    params(("key" = String, Path, description = "id or name"), DeleteImportQuery),
    responses(
        (status = 202, body = Resource<TaskSpec, TaskStatus>),
        (status = 200, body = VmImportForgotten, description = "force: only the resource was deleted"),
        (status = "4XX", body = crate::error::ErrorBody),
    ))]
pub async fn delete_import(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(q): Query<DeleteImportQuery>,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Write, VmImportSpec::KIND)?;
    let import = lookup::<VmImportSpec>(&state.cluster, &key)?;
    check_idle(&state, &import)?;
    if let Err(e) = check_online(&state, import.spec.node) {
        if !q.force {
            return Err(e);
        }
        tracing::warn!(import = %import.name, node = %import.spec.node, "deleting vm-import without its files");
        state
            .cluster
            .delete::<VmImportSpec>(import.id, None)
            .await?;
        let message = format!(
            "vm-import {:?} deleted; its files are left on node {:?} ({})",
            import.name,
            node_name(&state, import.spec.node)?,
            e.message
        );
        return Ok(Json(VmImportForgotten { message }).into_response());
    }
    let task = create_task(
        &state.cluster,
        TaskOp::VmImportDelete,
        import.id,
        import.spec.node,
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(task)).into_response())
}

#[cfg(test)]
mod tests {
    use opl_types::vm::{Firmware, NicModel};
    use opl_types::vm_import::{SourceDisk, SourceNic};

    use super::*;

    fn source() -> SourceVm {
        let disk = |i: usize| SourceDisk {
            name: format!("disk{i}"),
            file: format!("vm-disk{i}.vmdk"),
            capacity_bytes: 1 << 30,
            controller: "scsi".into(),
        };
        let nic = |network: &str, mac: Option<&str>| SourceNic {
            network: network.into(),
            adapter: "vmxnet3".into(),
            mac: mac.map(|m| m.parse().unwrap()),
        };
        SourceVm {
            name: "vm".into(),
            guest_os: "ubuntu64Guest".into(),
            firmware: Firmware::Uefi,
            vcpus: 4,
            cores_per_socket: 2,
            memory_mib: 2048,
            disks: vec![disk(0), disk(1)],
            nics: vec![
                nic("VM Network", Some("00:50:56:8a:12:34")),
                nic("Backup", None),
            ],
        }
    }

    fn run(networks: &[(&str, &str)]) -> ImportRun {
        ImportRun {
            name: "web".into(),
            pool: "local".into(),
            networks: networks
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            disk_bus: None,
            nic_model: None,
            firmware: None,
            sockets: None,
            cores: None,
            memory_mib: None,
            keep_macs: true,
        }
    }

    #[test]
    fn import_spec_takes_the_proposal_unless_the_run_overrides_it() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let volumes = vec!["web-disk0".to_string(), "web-disk1".to_string()];
        let mut r = run(&[("VM Network", "lan"), ("Backup", "user")]);
        let spec = import_spec(&src, &proposal, &r, &volumes).unwrap();
        assert_eq!((spec.cpu.sockets, spec.cpu.cores), (2, 2));
        assert_eq!(spec.cpu.model, CpuModel::Host);
        assert_eq!(spec.memory_mib, 2048);
        assert_eq!(spec.firmware, Firmware::Uefi);
        assert_eq!(spec.machine, MachineType::Q35);
        assert_eq!(spec.power, PowerState::Stopped);
        assert_eq!(spec.node, None);
        let disks: Vec<(&str, DiskBus, Option<u8>)> = spec
            .disks
            .iter()
            .map(|d| (d.volume.as_str(), d.bus, d.boot_order))
            .collect();
        assert_eq!(
            disks,
            [
                ("web-disk0", DiskBus::VirtioBlk, Some(1)),
                ("web-disk1", DiskBus::VirtioBlk, None)
            ]
        );
        let nics: Vec<(&str, NicModel, Option<String>)> = spec
            .nics
            .iter()
            .map(|n| (n.network.as_str(), n.model, n.mac.map(|m| m.to_string())))
            .collect();
        assert_eq!(
            nics,
            [
                (
                    "lan",
                    NicModel::Virtio,
                    Some("00:50:56:8a:12:34".to_string())
                ),
                ("user", NicModel::Virtio, None)
            ]
        );

        r.disk_bus = Some(DiskBus::Sata);
        r.nic_model = Some(NicModel::E1000e);
        r.firmware = Some(Firmware::Bios);
        r.sockets = Some(1);
        r.cores = Some(4);
        r.memory_mib = Some(4096);
        r.keep_macs = false;
        let spec = import_spec(&src, &proposal, &r, &volumes).unwrap();
        assert_eq!((spec.cpu.sockets, spec.cpu.cores), (1, 4));
        assert_eq!(spec.memory_mib, 4096);
        assert_eq!(spec.firmware, Firmware::Bios);
        assert!(spec.disks.iter().all(|d| d.bus == DiskBus::Sata));
        assert!(spec.nics.iter().all(|n| n.model == NicModel::E1000e));
        assert!(spec.nics.iter().all(|n| n.mac.is_none()));
    }

    #[test]
    fn import_spec_needs_a_mapping_for_every_source_network() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let err = import_spec(&src, &proposal, &run(&[("VM Network", "lan")]), &[]).unwrap_err();
        assert_eq!(err, "no network mapping for \"Backup\"");
        // 원본 NIC이 쓰지 않는 매핑은 무시한다.
        let r = run(&[("VM Network", "lan"), ("Backup", "lan"), ("other", "x")]);
        let spec = import_spec(&src, &proposal, &r, &[]).unwrap();
        assert_eq!(spec.nics.len(), 2);
    }

    #[test]
    fn import_spec_falls_back_to_the_proposed_network() {
        let src = source();
        // 이름이 같은 opLite 네트워크 `Backup`이 있다.
        let (proposal, _) = propose(&src, &["Backup".to_string()]);
        let spec = import_spec(&src, &proposal, &run(&[("VM Network", "lan")]), &[]).unwrap();
        let networks: Vec<&str> = spec.nics.iter().map(|n| n.network.as_str()).collect();
        assert_eq!(networks, ["lan", "Backup"]);
        // 요청의 매핑이 제안보다 앞선다.
        let r = run(&[("VM Network", "lan"), ("Backup", "user")]);
        let spec = import_spec(&src, &proposal, &r, &[]).unwrap();
        assert_eq!(spec.nics[1].network, "user");
        // 제안에도 없으면 오류다.
        let err = import_spec(&src, &proposal, &run(&[]), &[]).unwrap_err();
        assert_eq!(err, "no network mapping for \"VM Network\"");
    }

    #[test]
    fn import_spec_puts_at_most_six_disks_on_sata() {
        let src = source();
        let (proposal, _) = propose(&src, &[]);
        let mut r = run(&[("VM Network", "lan"), ("Backup", "lan")]);
        r.disk_bus = Some(DiskBus::Sata);
        let volumes: Vec<String> = (0..7).map(|i| format!("web-disk{i}")).collect();
        let err = import_spec(&src, &proposal, &r, &volumes).unwrap_err();
        assert_eq!(err, "7 disks do not fit on SATA (6 ports); use virtio-blk");
        assert!(import_spec(&src, &proposal, &r, &volumes[..6]).is_ok());
        r.disk_bus = Some(DiskBus::VirtioBlk);
        assert!(import_spec(&src, &proposal, &r, &volumes).is_ok());
    }

    /// 만들기 전에 넣은 자격 증명은 만들기를 끝내지 못하면(guard가 그대로 버려지면) 지워진다.
    #[test]
    fn credential_guard_removes_unfinished_credentials() {
        let credentials = Arc::new(ImportCredentials::default());
        let conn = EsxiConnection {
            host: "esxi.example".into(),
            port: 443,
            user: "root".into(),
            password: "pw".into(),
            datacenter: "ha-datacenter".into(),
            fingerprint: None,
        };
        let (failed, created) = (Uuid::now_v7(), Uuid::now_v7());
        let guard = CredentialGuard::insert(credentials.clone(), failed, conn.clone());
        assert!(credentials.get(failed).is_some());
        drop(guard);
        assert!(credentials.get(failed).is_none());
        let mut guard = CredentialGuard::insert(credentials.clone(), created, conn);
        guard.armed = false;
        drop(guard);
        assert!(credentials.get(created).is_some());
    }
}
