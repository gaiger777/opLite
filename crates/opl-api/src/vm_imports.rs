//! VM 가져오기. 올린 OVA를 읽어([`ova`]·[`ovf`]) 원본 VM을 알아내고 opLite VM 설정을 제안한다.
//! 원본은 올린 노드의 `{upload_dir}/imports/{가져오기 id}/source.ova`에 둔다.

pub mod ova;
pub mod ovf;

use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::Cluster;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::vm_import::{
    ImportPhase, ImportSource, OvaSource, VmImport, VmImportSpec, VmImportStatus, propose,
};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::check_name;
use crate::state::AppState;
use crate::volumes::{ImageFile, check_upload_space, content_length, receive};
use ova::{OvaError, parse_ova};

/// `/vm-imports/ova` 경로와 겹치므로 가져오기 이름으로 쓸 수 없다.
const RESERVED_NAME: &str = "ova";
/// 가져오기 디렉터리 안의 원본 OVA 파일 이름
pub(crate) const SOURCE_OVA: &str = "source.ova";

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

/// OVA를 받기 전의 확인. 사전 확인과 올리기가 함께 쓴다. 확인 뒤에 다른 요청이 이름을 가져가면
/// 저장소에 만들 때 `name_taken`이 난다.
async fn check_upload(
    state: &AppState,
    principal: &Principal,
    name: &str,
    size: u64,
) -> Result<(), ApiError> {
    state.authorize(principal, Action::Write, VmImportSpec::KIND)?;
    check_name(name)?;
    if name == RESERVED_NAME {
        return Err(ApiError::bad_request(format!(
            "the name {RESERVED_NAME} is reserved"
        )));
    }
    // 원본은 올린 노드에 남고 가져오기도 그 노드에서 하므로 VM을 돌리는 노드여야 한다.
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

/// 이전 opld가 남긴 가져오기 파일을 정리한다(노드가 시작할 때, API를 띄우기 전). 리소스가 없는
/// `imports/{id}` 디렉터리를 지우고, 이 노드의 `ready` 가져오기인데 원본이 없으면 `failed`로 바꾼다.
pub(crate) async fn sweep(cluster: &Cluster, node: Uuid, upload_dir: &FsPath) {
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
        if import.spec.node != node
            || import.status.phase != ImportPhase::Ready
            || import_dir(upload_dir, import.id).join(SOURCE_OVA).exists()
        {
            continue;
        }
        tracing::warn!(import = %import.name, "the source files of the import are gone");
        let status = VmImportStatus {
            phase: ImportPhase::Failed,
            error: Some("source files are gone".into()),
            ..import.status.clone()
        };
        if let Err(e) = cluster
            .update_status_checked::<VmImportSpec>(import.id, import.revision, &status)
            .await
        {
            tracing::warn!(import = %import.name, error = %e, "cannot mark the import failed");
        }
    }
}
