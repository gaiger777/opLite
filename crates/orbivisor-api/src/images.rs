//! 이미지 라이브러리 API(계획 58 §8): 내장·관리자 카탈로그 목록, 관리자 항목 더하기·지우기, 카탈로그 이미지 받기
//! (→ 템플릿 한 번에). 관리자 항목은 클러스터 설정(`SettingsSpec.image_catalog`)에 둔다.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use orbivisor_types::cloud_init::{CloudInit, IpConfig, PASSWORD_MASK, is_crypt_hash};
use orbivisor_types::image_catalog::{
    CatalogError, CatalogImage, Checksum, builtin, find, validate_custom,
};
use orbivisor_types::network::{NetworkSpec, USER_NETWORK};
use orbivisor_types::node::{INSTALL_MEDIA, NodeSpec, TEMPLATES};
use orbivisor_types::resource::ResourceSpec;
use orbivisor_types::settings::SettingsSpec;
use orbivisor_types::storage::{
    StoragePool, VolumeContent, VolumePhase, VolumeSpec, pool_usable_on,
};
use orbivisor_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use orbivisor_types::template::{
    DEFAULT_TEMPLATE_MEMORY_MIB, DEFAULT_TEMPLATE_VCPUS, TemplateDraft,
};
use orbivisor_types::vm::{Arch, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{List, check_name};
use crate::state::AppState;
use crate::vms::crypt_password;
use crate::volumes::{
    DownloadCheck, check_pool_format, check_volume_name, default_format, find_pool, pool_node,
    redact_url, start_download,
};

/// 목록의 카탈로그 항목: 이미지와 출처, 이미 받은 볼륨
#[derive(Debug, Serialize, ToSchema)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub image: CatalogImage,
    pub source: CatalogSource,
    /// 이 이미지 URL에서 받아 준비된 볼륨 이름(끝난 `volume-download` Task의 결과, 이름 순)
    pub volumes: Vec<String>,
}

/// 카탈로그 항목의 출처
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CatalogSource {
    /// Orbivisor에 든 목록
    Builtin,
    /// 관리자가 더한 항목(`settings.image_catalog`)
    Custom,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CatalogQuery {
    /// 이 아키텍처의 항목만
    #[serde(default)]
    #[param(inline)]
    pub arch: Option<Arch>,
}

/// 내장 항목과 관리자 항목(그 순서)
fn catalog(state: &AppState) -> Result<Vec<(CatalogImage, CatalogSource)>, ApiError> {
    let custom = crate::settings::current(&state.cluster)?.spec.image_catalog;
    Ok(builtin()
        .into_iter()
        .map(|i| (i, CatalogSource::Builtin))
        .chain(custom.into_iter().map(|i| (i, CatalogSource::Custom)))
        .collect())
}

/// 설정 쓰기 권한(카탈로그 항목을 더하고 지우는 관리자)이 있는지
pub(crate) fn writes_settings(state: &AppState, principal: &Principal) -> bool {
    state
        .authorize(principal, Action::Write, SettingsSpec::KIND)
        .is_ok()
}

/// 관리자가 아닌 주체에게 낼 카탈로그 항목: 이미지·체크섬 파일 URL의 쿼리 문자열(서명·토큰)과 사용자 정보를 가린다
/// ([`redact_url`]). 받기는 저장된 URL을 쓴다.
pub(crate) fn redact_image(image: &mut CatalogImage) {
    image.url = redact_url(&image.url);
    if let Checksum::Sums { url } = &mut image.checksum {
        *url = redact_url(url);
    }
}

/// 이미지 카탈로그: 내장 항목과 관리자 항목. `arch`를 주면 그 아키텍처만. 항목마다 그 URL에서 이미 받아 준비된 볼륨을
/// `volumes`로 함께 준다. 관리자(설정 쓰기 권한)가 아니면 URL의 쿼리 문자열과 사용자 정보를 가린다.
#[utoipa::path(get, path = "/image-catalog", tag = "storage",
    params(CatalogQuery),
    responses((status = 200, body = List<CatalogEntry>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_catalog(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<CatalogQuery>,
) -> Result<Json<List<CatalogEntry>>, ApiError> {
    state
        .authz
        .require_root(&principal, orbivisor_types::authz::Privilege::SystemRead)?;
    let images = catalog(&state)?;
    let admin = writes_settings(&state, &principal);
    let (revision, tasks) = state.cluster.list::<TaskSpec>()?;
    let (_, volumes) = state.cluster.list::<VolumeSpec>()?;
    let items = images
        .into_iter()
        .filter(|(i, _)| q.arch.is_none_or(|a| a == i.arch))
        .map(|(image, source)| {
            let mut names: Vec<String> = tasks
                .iter()
                .filter(|t| t.status.state.is_finished())
                .filter(|t| matches!(&t.spec.op, TaskOp::VolumeDownload { url, .. } if *url == image.url))
                .filter_map(|t| volumes.iter().find(|v| v.id == t.spec.target))
                .filter(|v| v.status.phase == VolumePhase::Ready)
                .map(|v| v.name.clone())
                .collect();
            names.sort();
            names.dedup();
            let mut image = image;
            if !admin {
                redact_image(&mut image);
            }
            CatalogEntry {
                image,
                source,
                volumes: names,
            }
        })
        .collect();
    Ok(Json(List { revision, items }))
}

/// 고정 해시는 소문자로 둔다(체크섬 파일이 돌려주는 모양과 같게).
fn normalize(mut image: CatalogImage) -> CatalogImage {
    if let Checksum::Sha256 { value } | Checksum::Sha512 { value } = &mut image.checksum {
        *value = value.to_ascii_lowercase();
    }
    image
}

/// 카탈로그 검사 오류: 같은 `(id, arch)`는 409 `already_exists`, 나머지는 400
fn catalog_error(e: CatalogError) -> ApiError {
    match e {
        CatalogError::Duplicate(..) => ApiError::conflict("already_exists", e.to_string()),
        e => ApiError::bad_request(e.to_string()),
    }
}

/// 관리자 카탈로그 항목을 더한다(201). 같은 `(id, arch)`가 있으면 409 `already_exists`, 내장 항목의 id(아키텍처와
/// 상관없이)·잘못된 id·URL·해시 길이·200개 넘음은 400이다. 고정 해시는 소문자로 저장한다. 모든 노드가 `templates`를
/// 알아야 한다(409 `cluster_upgrading`).
#[utoipa::path(post, path = "/image-catalog", tag = "storage",
    request_body = CatalogImage,
    responses((status = 201, body = CatalogImage), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn add_catalog_image(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(image): Json<CatalogImage>,
) -> Result<(StatusCode, Json<CatalogImage>), ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    require_feature(&state.cluster, TEMPLATES)?;
    let image = normalize(image);
    image.validate().map_err(catalog_error)?;
    crate::settings::write(&state.cluster, None, |spec| {
        let mut images = spec.image_catalog.clone();
        images.push(image.clone());
        validate_custom(&images).map_err(catalog_error)?;
        spec.image_catalog = images;
        Ok(())
    })
    .await?;
    audit.detail(format!("image catalog add {}", image.id));
    Ok((StatusCode::CREATED, Json(image)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CatalogRemoveQuery {
    /// 이 아키텍처의 항목만 지운다. 없으면 그 id의 모든 항목이다.
    #[serde(default)]
    #[param(inline)]
    pub arch: Option<Arch>,
}

/// 관리자 카탈로그 항목을 지운다(204). 이미 받은 볼륨과 템플릿은 그대로다. 내장 항목은 지울 수 없고(400), 없으면 404다.
#[utoipa::path(delete, path = "/image-catalog/{id}", tag = "storage",
    params(("id" = String, Path, description = "catalog image id"), CatalogRemoveQuery),
    responses((status = 204, description = "Removed"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn remove_catalog_image(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
    Query(q): Query<CatalogRemoveQuery>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    if builtin().iter().any(|i| i.id == id) {
        return Err(ApiError::bad_request(format!(
            "image {id:?} is a built-in image and cannot be removed"
        )));
    }
    crate::settings::write(&state.cluster, None, |spec| {
        let before = spec.image_catalog.len();
        spec.image_catalog
            .retain(|i| !(i.id == id && q.arch.is_none_or(|a| a == i.arch)));
        if spec.image_catalog.len() == before {
            return Err(ApiError::not_found(format!(
                "image {id:?} not found in the catalog"
            )));
        }
        Ok(())
    })
    .await?;
    audit.detail(format!("image catalog remove {id}"));
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /image-catalog/{id}/download`
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogDownloadRequest {
    /// 받은 디스크를 둘 풀
    pub pool: String,
    /// 볼륨 이름. 없으면 id의 `.`을 `-`로 바꾸고 aarch64면 `-arm64`를 붙인다(`ubuntu-24-04`).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// 받을 항목의 아키텍처. 없으면 풀에서 받는 노드의 아키텍처다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub arch: Option<Arch>,
    /// 받은 뒤 만들 템플릿 VM. 없으면 볼륨만 받는다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub template: Option<CatalogTemplate>,
}

/// 카탈로그 이미지를 받은 뒤 만들 템플릿 VM. 머신 타입은 항목의 아키텍처(`q35`·`virt`), 펌웨어는 항목의 것이고, 디스크는
/// `virtio-blk` 첫 부팅 디스크, NIC 하나, cloud-init(호스트 이름·정적 IP 없음)이다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogTemplate {
    /// 템플릿 VM 이름
    pub name: String,
    #[serde(default = "default_vcpus")]
    #[schema(default = 2)]
    pub vcpus: u16,
    #[serde(default = "default_memory")]
    #[schema(default = 2048)]
    pub memory_mib: u32,
    /// 받은 디스크를 이 크기로 늘린다(바이트, 512의 배수, 이미지 크기 이상). 없으면 이미지 크기 그대로다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub size_bytes: Option<u64>,
    /// NIC 하나의 네트워크 이름
    pub network: String,
    /// cloud-init 기본 설정(사용자·비밀번호·SSH 키 등). 평문 비밀번호는 SHA-512 crypt로 바꿔 Task에 담는다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cloud_init: Option<CloudInit>,
    /// CPU 모델(계획 63, 없으면 `host`). 별칭은 버전 모델로 고정된다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cpu_model: Option<orbivisor_types::cpu::CpuModel>,
    /// CPU 플래그(`["-avx512vbmi"]`, 계획 63)
    #[serde(default)]
    #[schema(default)]
    pub cpu_flags: Vec<orbivisor_types::cpu::CpuFlag>,
    /// CPU 정책(계획 64, 없으면 `shared`). `dedicated`는 권한 `vm.dedicated`가 든다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cpu_policy: Option<orbivisor_types::numa::CpuPolicy>,
    /// 전용 CPU 스레드 정책(계획 64)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cpu_thread_policy: Option<orbivisor_types::numa::ThreadPolicy>,
    /// 에뮬레이터 스레드 정책(계획 64)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cpu_emulator: Option<orbivisor_types::numa::EmulatorPolicy>,
    /// vNUMA(계획 64)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub numa: Option<orbivisor_types::numa::NumaSpec>,
    /// hugepages(계획 64). 권한 `vm.dedicated`가 든다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub hugepages: Option<orbivisor_types::numa::PageSize>,
}

fn default_vcpus() -> u16 {
    DEFAULT_TEMPLATE_VCPUS
}

fn default_memory() -> u32 {
    DEFAULT_TEMPLATE_MEMORY_MIB
}

/// 카탈로그 이미지를 풀에 받는 Task(`volume-download`)를 돌려준다(202). 항목의 체크섬(고정 해시나 체크섬 파일)으로
/// 확인하고, `template`이 있으면 가져온 뒤 디스크를 늘리고 꺼진 템플릿 VM을 만든다(그 단계가 실패해도 받은 볼륨은
/// 남는다). 항목이 없으면 404, 볼륨이나 VM 이름이 쓰였으면 409 `name_taken`, 템플릿의 네트워크가 없거나 크기가
/// 이미지보다 작으면 400, 풀을 쓸 수 있는 노드 가운데 항목의 아키텍처인 노드가 없으면 400 `machine_arch_mismatch`다.
/// 볼륨 쓰기 권한(템플릿이면 VM 쓰기도)이 있어야 하고, 모든 노드가 `templates`를 알아야 한다(409 `cluster_upgrading`).
#[utoipa::path(post, path = "/image-catalog/{id}/download", tag = "storage",
    params(("id" = String, Path, description = "catalog image id")),
    request_body = CatalogDownloadRequest,
    responses((status = 202, body = orbivisor_types::resource::Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn download_catalog_image(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(id): Path<String>,
    Json(req): Json<CatalogDownloadRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    require_feature(&state.cluster, INSTALL_MEDIA)?;
    require_feature(&state.cluster, TEMPLATES)?;
    let pool = find_pool(&state, &req.pool)?;
    // 받을 풀의 `storage.allocate`, 템플릿을 만들면 루트(VM 트리의 루트에 만든다)의 `vm.create`
    state.authz.require(
        &principal,
        orbivisor_types::authz::Privilege::StorageAllocate,
        &pool,
    )?;
    if req.template.is_some() {
        state
            .authz
            .require_root(&principal, orbivisor_types::authz::Privilege::VmCreate)?;
    }
    let format = default_format(&pool);
    check_pool_format(&pool, format)?;
    let arch = match req.arch {
        Some(arch) => arch,
        None => {
            let node = pool_node(&state, &pool)?;
            state
                .cluster
                .get::<NodeSpec>(node)?
                .and_then(|n| n.status.arch)
                .unwrap_or_else(Arch::native)
        }
    };
    let images: Vec<CatalogImage> = catalog(&state)?.into_iter().map(|(i, _)| i).collect();
    let Some(image) = find(&images, &id, arch) else {
        return Err(ApiError::not_found(format!(
            "image {id:?} ({}) not found in the catalog",
            arch_name(arch)
        )));
    };
    let name = req
        .name
        .clone()
        .unwrap_or_else(|| image.default_volume_name());
    check_volume_name(&name)?;
    if let Some(t) = &req.template {
        crate::cpu::check_flag_privilege(&state, &principal, &t.cpu_flags, &[])?;
        // 전용 CPU·hugepages 템플릿은 `vm.dedicated`(템플릿은 루트에 만든다, 계획 64)
        let host = t.cpu_policy.is_some_and(|p| !p.is_shared()) || t.hugepages.is_some();
        crate::numa::require_privilege(&state, &principal, host, None)?;
    }
    let template = match &req.template {
        Some(t) => Some(Box::new(draft(&state, &pool, image, &name, t)?)),
        None => None,
    };
    let (sha256, sha512, sums) = match &image.checksum {
        Checksum::Sha256 { value } => (Some(value.as_str()), None, None),
        Checksum::Sha512 { value } => (None, Some(value.as_str()), None),
        Checksum::Sums { url } => (None, None, Some(url.as_str())),
    };
    let check = DownloadCheck::parse(sha256, sha512, sums)?;
    let mut detail = format!("image download {} to pool {}", image.id, pool.name);
    if let Some(t) = &template {
        detail.push_str(&format!(" (template {})", t.name));
        let asked = req.template.as_ref().and_then(|r| r.cpu_model.as_ref());
        if let Some(cpu) =
            crate::cpu::audit_requested_cpu(t.cpu_model.as_ref(), asked, &t.cpu_flags)
        {
            detail.push_str(&format!(" {cpu}"));
        }
    }
    let op = TaskOp::VolumeDownload {
        url: image.url.clone(),
        sha256: check.sha256,
        source_format: image.source_format,
        sha512: check.sha512,
        checksum_url: check.checksum_url,
        template,
    };
    let task = start_download(&state, name, &pool, format, VolumeContent::Disk, op).await?;
    audit.detail(detail);
    Ok((StatusCode::ACCEPTED, Json(task)))
}

fn arch_name(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
    }
}

/// 템플릿 요청 `t`를 Task에 담을 초안으로: VM 만들기와 같은 검사(이름, 이름이 쓰였으면 409 `name_taken`, 네트워크,
/// 크기, 아키텍처가 맞는 노드)를 하고 머신·펌웨어를 항목에서 채우며 cloud-init 비밀번호를 해시한다. 호스트 이름·고정 IP는
/// 400이다(§8 — 템플릿은 여러 VM의 원본).
fn draft(
    state: &AppState,
    pool: &StoragePool,
    image: &CatalogImage,
    volume: &str,
    t: &CatalogTemplate,
) -> Result<TemplateDraft, ApiError> {
    check_name(&t.name)?;
    if state.cluster.get_by_name::<VmSpec>(&t.name)?.is_some() {
        return Err(ApiError::conflict(
            "name_taken",
            format!("VM {:?} already exists", t.name),
        ));
    }
    if t.network != USER_NETWORK
        && state
            .cluster
            .get_by_name::<NetworkSpec>(&t.network)?
            .is_none()
    {
        return Err(ApiError::bad_request(format!(
            "network {:?} not found",
            t.network
        )));
    }
    if let Some(size) = t.size_bytes {
        if size % 512 != 0 {
            return Err(ApiError::bad_request(
                "template.size_bytes must be a multiple of 512",
            ));
        }
        if size < image.min_size_bytes {
            return Err(ApiError::bad_request(format!(
                "template.size_bytes must be at least {} bytes (the size of image {:?})",
                image.min_size_bytes, image.id
            )));
        }
    }
    // 템플릿에서 배포한 VM은 풀을 쓸 수 있는 노드에서 켠다(연결 복제는 같은 풀). 그런 노드 가운데 항목의 아키텍처인
    // 노드가 있어야 한다(arch를 모르는 옛 노드는 막지 않는다).
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let fits = nodes.iter().any(|n| {
        !n.spec.witness && pool_usable_on(pool, n) && n.status.arch.is_none_or(|a| a == image.arch)
    });
    if !fits {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "machine_arch_mismatch",
            format!(
                "image {:?} is for {}, but no node that can use pool {:?} is {}",
                image.id,
                arch_name(image.arch),
                pool.name,
                arch_name(image.arch)
            ),
        ));
    }
    let mut cloud_init = t.cloud_init.clone();
    // 템플릿은 여러 VM의 원본이므로 호스트 이름·고정 IP를 두지 않는다(설계 §8). 배포할 때 VM마다 준다.
    if let Some(ci) = &cloud_init {
        if ci.hostname.is_some() {
            return Err(ApiError::bad_request(
                "template.cloud_init.hostname: a template has no host name; give it when deploying",
            ));
        }
        let fixed = ci
            .networks
            .iter()
            .flat_map(|n| [&n.ipv4, &n.ipv6])
            .any(|ip| matches!(ip, Some(IpConfig::Static { .. })));
        if fixed {
            return Err(ApiError::bad_request(
                "template.cloud_init.networks: a template has no static IP address; give it when deploying",
            ));
        }
    }
    if let Some(password) = cloud_init.as_mut().and_then(|c| c.password.as_mut()) {
        if password == PASSWORD_MASK {
            return Err(ApiError::bad_request(
                "template.cloud_init.password: give a new password",
            ));
        }
        if !is_crypt_hash(password) {
            *password = crypt_password(password);
        }
    }
    let draft = TemplateDraft {
        name: t.name.clone(),
        vcpus: t.vcpus,
        memory_mib: t.memory_mib,
        size_bytes: t.size_bytes,
        network: t.network.clone(),
        cloud_init,
        machine: Some(image.arch.machine()),
        firmware: Some(image.firmware),
        cpu_model: t.cpu_model.clone(),
        cpu_flags: t.cpu_flags.clone(),
        cpu_policy: t.cpu_policy,
        cpu_thread_policy: t.cpu_thread_policy,
        cpu_emulator: t.cpu_emulator,
        numa: t.numa.clone(),
        hugepages: t.hugepages,
    };
    let mut draft = draft;
    let model = draft.cpu_model.clone().unwrap_or_default();
    crate::cpu::require_cpu_feature(&state.cluster, &model, &draft.cpu_flags)?;
    // CPU 정책·vNUMA·hugepages(계획 64)는 옛 노드가 풀지 못한다(검증보다 먼저).
    if draft.cpu_policy.is_some()
        || draft.cpu_thread_policy.is_some()
        || draft.cpu_emulator.is_some()
        || draft.numa.is_some()
        || draft.hugepages.is_some()
    {
        require_feature(&state.cluster, orbivisor_types::node::NUMA)?;
    }
    let spec = draft.vm_spec(volume).map_err(|e| match e {
        orbivisor_types::template::TemplateError::Invalid(v) => crate::cpu::validation_error(v),
        e => ApiError::bad_request(format!("template: {e}")),
    })?;
    // 별칭은 버전 모델로 고정해 Task에 넣는다(계획 63 §8).
    if draft.cpu_model.is_some() || !draft.cpu_flags.is_empty() {
        let inv = crate::scheduler::Inventory::load(&state.cluster)?;
        let pinned = crate::cpu::check_cpu(&inv, image.arch, &spec.cpu, None)?;
        if draft.cpu_model.is_some() {
            draft.cpu_model = Some(pinned.model);
        }
    }
    Ok(draft)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 계획 73 리뷰 1: 관리자 아닌 주체에게 가리는 카탈로그 URL의 서명·사용자 정보는 설정 이력에도 평문으로 남지 않는다.
    #[test]
    fn history_masks_what_the_catalog_view_hides() {
        let image: CatalogImage = serde_json::from_value(serde_json::json!({
            "id": "signed", "title": "Signed", "os": "ubuntu", "version": "24.04", "arch": "x86_64",
            "url": "https://u:PASSWORD@s3.example.com/img.qcow2?X-Amz-Signature=SIGNATURE",
            "checksum": { "kind": "sums", "url": "https://s3.example.com/SUMS?token=SUMSTOKEN" },
            "source_format": "qcow2", "firmware": "uefi"
        }))
        .unwrap();
        let mut shown = image.clone();
        redact_image(&mut shown);
        let spec = serde_json::json!({ "image_catalog": [image] });
        let masked = orbivisor_types::history::history_mask(
            orbivisor_types::resource::Kind::Settings,
            None,
            &spec,
        );
        assert_eq!(masked["image_catalog"][0]["url"], shown.url);
        let Checksum::Sums { url } = &shown.checksum else {
            panic!("sums")
        };
        assert_eq!(masked["image_catalog"][0]["checksum"]["url"], *url);
        for secret in ["PASSWORD", "SIGNATURE", "SUMSTOKEN"] {
            assert!(spec.to_string().contains(secret));
            assert!(!masked.to_string().contains(secret), "{masked}");
        }
    }
}
