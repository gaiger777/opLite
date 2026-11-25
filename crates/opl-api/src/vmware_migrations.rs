//! VMware 이전 계획 `/api/v1/vmware-migrations`와 항목 `/api/v1/vmware-vms`(계획 72 설계 §8.3~§8.5): 원본 연결의
//! 인벤토리에서 고른 VM 여러 대를 같은 매핑 규칙으로 옮기는 계획을 만들고(항목 `vmware-vm`마다 VM 하나), 바꾸고, 지우고,
//! 시작·멈춤·이어 가기·취소의 상태를 바꾸고, VM마다 미리 확인한다. 복사·전환 Task는 스테이지 4·5다.
//!
//! - **권한**: 계획·항목은 루트 종류다(읽기 `system.read`). 만들기·바꾸기·지우기·미리 확인·동작은 루트 `vm.create`(경로표)이고,
//!   규칙·덮어쓰기가 가리키는 폴더는 그 폴더의 `vm.create`(Mirror는 `inventory.folder`도)를 본다. 미리 확인은 VM마다 계획 71
//!   `vm-checks`의 검사(대상 폴더 `vm.create` 먼저, 첫 거절에서 멈춤 — 숨은 풀·네트워크의 성질이 새지 않게)에 VMware 쪽 표와
//!   게스트 힌트를 더한다.
//! - **겹침**: 한 원본 VM(`instance_uuid`)은 끝나지 않은 항목 하나에만 든다(Raft 독점 키 → 409 `vm_in_migration`).

pub(crate) mod plan;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use opl_types::authz::Privilege;
use opl_types::network::NetworkSpec;
use opl_types::node::{FOLDERS, VMWARE_MIGRATE};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::storage::StoragePoolSpec;
use opl_types::vm::VmSpec;
use opl_types::vm_check::{VmCheckLevel, VmCheckProblem};
use opl_types::vmware::{
    MAX_MIGRATION_VMS, VmwareCutoverPolicy, VmwareFolderRule, VmwareLimits, VmwareMigration,
    VmwareMigrationMode, VmwareMigrationPhase, VmwareMigrationSpec, VmwareMigrationStatus,
    VmwareOverrides, VmwareRules, VmwareSource, VmwareSourceSpec, VmwareSyncPolicy, VmwareVm,
    VmwareVmSpec, VmwareVmState, VmwareVmStatus, item_name,
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
use crate::vm_checks::{VmCheckRequest, check_planned};
use crate::vmware::VsphereError;
use crate::vmware::inventory::{InvVm, Inventory};
use plan::{MapInput, guest_hints, map_vm, source_problems, target_folder};

// ---- 요청·응답 ----

/// 계획에 넣을 원본 VM
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationVmRequest {
    /// 원본 VM: managed object id(`vm-42`), 인벤토리 경로(`DC1/vm/Prod/web01`) 또는 `config.instanceUuid`
    #[serde(rename = "ref")]
    pub vm_ref: String,
    /// 만들 VM 이름. 없으면 원본 이름을 다듬은 것(소문자·`-`)이다.
    #[serde(default)]
    #[schema(nullable = false)]
    pub target_name: Option<String>,
    /// 이 VM만의 덮어쓰기(규칙보다 앞선다)
    #[serde(default)]
    #[schema(nullable = false)]
    pub overrides: Option<VmwareOverrides>,
}

/// 이전 계획 만들기 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateMigrationRequest {
    pub name: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// 원본 연결(`vmware-source`의 이름 또는 id)
    pub source: String,
    pub mode: VmwareMigrationMode,
    #[serde(default)]
    pub rules: VmwareRules,
    #[serde(default)]
    pub limits: VmwareLimits,
    #[serde(default)]
    pub sync: VmwareSyncPolicy,
    /// 전환 정책. `at`은 유닉스 초(CLI는 RFC 3339를 받아 바꾼다).
    #[serde(default)]
    pub cutover: VmwareCutoverPolicy,
    /// 옮길 VM(1~200대)
    pub vms: Vec<MigrationVmRequest>,
}

/// 계획에서 바꿀 값(`draft`·`paused`에서만). 주어진 것만 바꾼다.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MigrationPatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub rules: Option<VmwareRules>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub limits: Option<VmwareLimits>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub sync: Option<VmwareSyncPolicy>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub cutover: Option<VmwareCutoverPolicy>,
    /// 더할 VM
    #[serde(default)]
    pub add_vms: Vec<MigrationVmRequest>,
    /// 뺄 VM(항목 이름, 원본 moref·instanceUuid 또는 원본 VM 이름). 시작한 VM은 뺄 수 없다(취소한다).
    #[serde(default)]
    pub remove_vms: Vec<String>,
}

/// 계획과 그 항목
#[derive(Debug, Serialize, ToSchema)]
pub struct MigrationView {
    pub migration: Resource<VmwareMigrationSpec, VmwareMigrationStatus>,
    pub vms: Vec<Resource<VmwareVmSpec, VmwareVmStatus>>,
}

/// 미리 확인할 VM(없으면 계획의 모든 VM)
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreflightRequest {
    /// 항목 이름, 원본 moref·instanceUuid 또는 원본 VM 이름
    #[serde(default)]
    pub vms: Vec<String>,
    /// 노드 캐시(60초)를 쓰지 않고 인벤토리를 다시 읽는다.
    #[serde(default)]
    pub refresh: bool,
}

/// VM 하나의 미리 확인 판정
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PreflightLevel {
    Ok,
    /// 시작할 수 있지만 알아 둘 것이 있다.
    Warn,
    /// 시작하면 이 VM은 `failed`가 된다(나머지는 간다).
    Blocked,
}

/// VM 하나의 미리 확인
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PreflightVm {
    /// 항목(`vmware-vm`) 이름
    pub item: String,
    /// 원본 VM moref
    #[serde(rename = "ref")]
    pub source_ref: String,
    /// 원본 VM 이름
    pub name: String,
    pub level: PreflightLevel,
    /// 자리: 계획 71 `vm-checks`의 자리(`name`·`disks.N`·`nics.N`·`placement`·`quota` …)와 `folder`(대상 폴더 권한),
    /// `source`(VMware 쪽), `guest`(게스트 힌트)
    pub problems: Vec<VmCheckProblem>,
    /// 스케줄러가 고를 대상 노드(노드 이름을 볼 수 있을 때만)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub node: Option<String>,
    /// 만들 VM의 스펙(우리 쪽 검사를 통과했을 때 — 채운 머신)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub spec_preview: Option<VmSpec>,
}

/// 미리 확인의 답(문제가 있어도 200)
#[derive(Debug, Serialize, ToSchema)]
pub struct PreflightAnswer {
    /// 원본 인벤토리를 읽은 시각(유닉스 초)
    pub fetched_at: u64,
    pub vms: Vec<PreflightVm>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct ItemsQuery {
    /// 이 계획(이름 또는 id)의 항목만
    #[serde(default)]
    pub migration: Option<String>,
}

// ---- 도움 ----

fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("invalid_vmware_migration")
}

fn items_of(state: &AppState, migration: Uuid) -> Result<Vec<VmwareVm>, ApiError> {
    let (_, items) = state.cluster.list::<VmwareVmSpec>()?;
    let mut items: Vec<VmwareVm> = items
        .into_iter()
        .filter(|i| i.spec.migration == migration)
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

/// 항목 상태별 수
pub(crate) fn counts(items: &[VmwareVm]) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for i in items {
        *out.entry(i.status.state.as_str().to_string()).or_insert(0) += 1;
    }
    out
}

fn view(state: &AppState, migration: Uuid) -> Result<MigrationView, ApiError> {
    let migration = state
        .cluster
        .get::<VmwareMigrationSpec>(migration)?
        .ok_or_else(|| ApiError::not_found("vmware-migration not found"))?;
    let vms = items_of(state, migration.id)?;
    Ok(MigrationView { migration, vms })
}

/// 계획의 상태를 `revision`일 때만 바꾼다(다른 요청이 그새 바꿨으면 409 `migration_busy`).
async fn write_status(
    state: &AppState,
    m: &VmwareMigration,
    status: &VmwareMigrationStatus,
) -> Result<(), ApiError> {
    match state
        .cluster
        .update_status_checked::<VmwareMigrationSpec>(m.id, m.revision, status)
        .await
    {
        Ok(_) => Ok(()),
        Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => Err(ApiError::conflict(
            "migration_busy",
            format!(
                "vmware-migration {:?} was changed by another request; try again",
                m.name
            ),
        )),
        Err(e) => Err(e.into()),
    }
}

/// 항목 상태별 수를 다시 센다(상태는 그대로).
async fn recount(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let Some(m) = state.cluster.get::<VmwareMigrationSpec>(id)? else {
        return Ok(());
    };
    let status = VmwareMigrationStatus {
        counts: counts(&items_of(state, id)?),
        ..m.status.clone()
    };
    if status != m.status {
        state
            .cluster
            .update_status::<VmwareMigrationSpec>(id, &status)
            .await?;
    }
    Ok(())
}

/// 규칙·덮어쓰기가 가리키는 폴더의 권한: VM을 둘 폴더의 `vm.create`, Mirror는 그 아래 폴더를 만들 `inventory.folder`
fn check_folders(
    state: &AppState,
    principal: &Principal,
    rules: &VmwareRules,
    vms: &[MigrationVmRequest],
) -> Result<(), ApiError> {
    let mut folders: BTreeSet<Uuid> = vms
        .iter()
        .filter_map(|v| v.overrides.as_ref().and_then(|o| o.folder))
        .collect();
    let (into, mirror) = match rules.folders {
        VmwareFolderRule::Flat { into } => (into, None),
        VmwareFolderRule::Mirror { under } => (under, Some(under)),
    };
    folders.extend(into);
    if !folders.is_empty() || mirror.is_some_and(|u| u.is_some()) {
        require_feature(&state.cluster, FOLDERS)?;
    }
    for f in &folders {
        state
            .authz
            .require_in_folder(principal, Privilege::VmCreate, Kind::Vm, Some(*f))?;
    }
    if let Some(under) = mirror {
        state.authz.require_in_folder(
            principal,
            Privilege::InventoryFolder,
            Kind::Folder,
            under,
        )?;
    }
    Ok(())
}

/// 계획의 원본 연결
fn source_of(state: &AppState, m: &VmwareMigrationSpec) -> Result<VmwareSource, ApiError> {
    state
        .cluster
        .get::<VmwareSourceSpec>(m.source)?
        .ok_or_else(|| {
            ApiError::conflict(
                "source_missing",
                "the vmware-source of this migration no longer exists",
            )
        })
}

async fn inventory_of(
    state: &AppState,
    source: &VmwareSource,
    refresh: bool,
) -> Result<(Arc<Inventory>, u64), ApiError> {
    state
        .vmware_cache
        .inventory(source, refresh)
        .await
        .map_err(VsphereError::api_error)
}

/// 원본 VM을 moref·경로·instanceUuid로 찾는다.
fn find_vm<'a>(inv: &'a Inventory, key: &str) -> Option<&'a InvVm> {
    inv.vm(key).or_else(|| inv.vm_by_path(key)).or_else(|| {
        inv.vms
            .iter()
            .find(|v| v.instance_uuid.as_deref() == Some(key))
    })
}

/// 요청한 VM들로 항목을 짓는다(아직 저장하지 않음). 이름 겹침·없는 VM·같은 VM 둘은 400.
fn build_items(
    state: &AppState,
    migration: &VmwareMigration,
    inv: &Inventory,
    existing: &[VmwareVm],
    vms: &[MigrationVmRequest],
) -> Result<Vec<VmwareVm>, ApiError> {
    let mut targets: BTreeSet<String> = existing
        .iter()
        .map(|i| i.spec.target_name.clone())
        .collect();
    let mut uuids: BTreeSet<String> = existing
        .iter()
        .map(|i| i.spec.instance_uuid.clone())
        .collect();
    let mut names: BTreeSet<String> = existing.iter().map(|i| i.name.clone()).collect();
    let mut out = Vec::new();
    for req in vms {
        let vm = find_vm(inv, &req.vm_ref).ok_or_else(|| {
            ApiError::bad_request(format!(
                "source VM {:?} is not in the inventory",
                req.vm_ref
            ))
            .with_code("vmware_vm_not_found")
        })?;
        let instance_uuid = vm
            .instance_uuid
            .clone()
            .ok_or_else(|| invalid(format!("source VM {:?} has no instance UUID", vm.path)))?;
        if !uuids.insert(instance_uuid.clone()) {
            return Err(invalid(format!(
                "source VM {:?} is in the plan twice",
                vm.path
            )));
        }
        let target = plan::target_name(req.target_name.as_deref(), &vm.name)
            .map_err(|e| invalid(format!("target name for {:?}: {e}", vm.path)))?;
        check_name(&target)?;
        if !targets.insert(target.clone()) {
            return Err(ApiError::bad_request(format!(
                "two VMs in the plan would be named {target:?}; give target_name"
            ))
            .with_code("name_taken"));
        }
        let base = item_name(&migration.name, &vm.name);
        let mut name = base.clone();
        let mut n = 2;
        while names.contains(&name) || state.cluster.get_by_name::<VmwareVmSpec>(&name)?.is_some() {
            let suffix = format!("-{n}");
            name = format!(
                "{}{suffix}",
                base[..base.len().min(63 - suffix.len())].trim_end_matches('-')
            );
            n += 1;
        }
        names.insert(name.clone());
        let spec = VmwareVmSpec {
            migration: migration.id,
            source_ref: vm.moref.clone(),
            instance_uuid,
            target_name: target,
            overrides: req
                .overrides
                .clone()
                .filter(|o| *o != VmwareOverrides::default()),
            node: None,
        };
        spec.validate().map_err(invalid)?;
        let mut item: VmwareVm = Resource::new(name, spec);
        item.status.source = Some(vm.to_source());
        out.push(item);
    }
    Ok(out)
}

/// 항목들을 만든다. 하나라도 실패하면 만든 것을 지우고 그 오류(같은 VM이 다른 계획에 있으면 409 `vm_in_migration`).
async fn create_items(state: &AppState, items: &[VmwareVm]) -> Result<(), ApiError> {
    for (n, item) in items.iter().enumerate() {
        if let Err(e) = state.cluster.create(item).await {
            for made in &items[..n] {
                if let Err(d) = state.cluster.delete::<VmwareVmSpec>(made.id, None).await {
                    tracing::warn!(item = %made.name, error = %d, "cannot remove a vmware-vm item");
                }
            }
            return Err(match e {
                ClusterError::Rejected(Rejection::InUse { field, holder, .. })
                    if field == "vmware-vm" =>
                {
                    in_migration(state, item, holder)
                }
                e => e.into(),
            });
        }
    }
    Ok(())
}

/// 409 `vm_in_migration`: 원본 VM이 끝나지 않은 다른 항목(`holder`)에 있다.
fn in_migration(state: &AppState, item: &VmwareVm, holder: Uuid) -> ApiError {
    let vm = item
        .status
        .source
        .as_ref()
        .map_or_else(|| item.spec.source_ref.clone(), |s| s.vm.name.clone());
    let held = state.cluster.get::<VmwareVmSpec>(holder).ok().flatten();
    let migration = held
        .as_ref()
        .and_then(|h| {
            state
                .cluster
                .get::<VmwareMigrationSpec>(h.spec.migration)
                .ok()
                .flatten()
        })
        .map(|m| m.name);
    ApiError::conflict(
        "vm_in_migration",
        match (held, migration) {
            (Some(h), Some(m)) => format!(
                "source VM {vm:?} is already in vmware-migration {m:?} ({}); finish or cancel it first",
                h.name
            ),
            _ => format!("source VM {vm:?} is already in another vmware-migration"),
        },
    )
}

/// 항목이 원본·우리 쪽에 남긴 것이 없다(Task가 아무것도 하지 않았다).
fn holds_nothing(s: &VmwareVmStatus) -> bool {
    s.snapshot.is_none()
        && s.pending_snapshot.is_none()
        && !s.cbt_enabled_by_us
        && s.target_vm.is_none()
        && s.source_changes.is_empty()
        && s.disks.iter().all(|d| d.volume.is_none())
}

/// 시작하지 않은 항목(뺄 수 있다)
fn not_started(s: &VmwareVmStatus) -> bool {
    matches!(
        s.state,
        VmwareVmState::Draft | VmwareVmState::Pending | VmwareVmState::Failed
    ) && holds_nothing(s)
}

/// 항목을 이름·id·원본 moref·instanceUuid로, 그다음 원본 VM 이름으로 찾는다.
fn find_item<'a>(items: &'a [VmwareVm], key: &str) -> Option<&'a VmwareVm> {
    items
        .iter()
        .find(|i| {
            i.name == key
                || i.id.to_string() == key
                || i.spec.source_ref == key
                || i.spec.instance_uuid == key
        })
        .or_else(|| {
            items
                .iter()
                .find(|i| i.status.source.as_ref().is_some_and(|s| s.vm.name == key))
        })
}

fn mode_name(mode: VmwareMigrationMode) -> &'static str {
    match mode {
        VmwareMigrationMode::Warm => "warm",
        VmwareMigrationMode::Cold => "cold",
    }
}

// ---- 미리 확인 ----

/// 묻는 주체가 볼 수 있는 우리 네트워크 이름(제안이 같은 이름을 고를 후보)
fn visible_networks(state: &AppState, principal: &Principal) -> Result<Vec<String>, ApiError> {
    let (_, nets) = state.cluster.list::<NetworkSpec>()?;
    Ok(state
        .authz
        .filter(principal, nets)?
        .into_iter()
        .map(|n| n.name)
        .collect())
}

fn level_of(problems: &[VmCheckProblem]) -> PreflightLevel {
    if problems.iter().any(|p| p.level == VmCheckLevel::Error) {
        PreflightLevel::Blocked
    } else if problems.is_empty() {
        PreflightLevel::Ok
    } else {
        PreflightLevel::Warn
    }
}

/// 미리 확인 하나의 결과와 시작 때 적을 노드
struct Checked {
    vm: PreflightVm,
    node: Option<Uuid>,
}

/// VM 하나를 미리 확인한다(설계 §8.5): 우리 쪽(계획 71 `vm-checks` — 대상 폴더 `vm.create` 먼저, 첫 거절에서 멈춤), 그다음
/// VMware 쪽 표와 게스트 힌트(인벤토리 값 — 원본을 부르지 않는다).
async fn check_item(
    state: &AppState,
    principal: &Principal,
    m: &VmwareMigration,
    source: &VmwareSource,
    inv: &Inventory,
    networks: &[String],
    item: &VmwareVm,
) -> Result<Checked, ApiError> {
    let Some(vm) = inv.vm(&item.spec.source_ref).or_else(|| {
        inv.vms
            .iter()
            .find(|v| v.instance_uuid.as_deref() == Some(item.spec.instance_uuid.as_str()))
    }) else {
        return Ok(Checked {
            vm: PreflightVm {
                item: item.name.clone(),
                source_ref: item.spec.source_ref.clone(),
                name: item
                    .status
                    .source
                    .as_ref()
                    .map_or_else(String::new, |s| s.vm.name.clone()),
                level: PreflightLevel::Blocked,
                problems: vec![plan::problem(
                    "source",
                    VmCheckLevel::Error,
                    "vmware_vm_gone",
                    format!(
                        "source VM {:?} is no longer in the inventory",
                        item.spec.source_ref
                    ),
                )],
                node: None,
                spec_preview: None,
            },
            node: None,
        });
    };
    let format = |pool: &str| match state.cluster.get_by_name::<StoragePoolSpec>(pool) {
        Ok(Some(p)) => crate::volumes::default_format(&p),
        _ => opl_types::runtime::ImageFormat::Qcow2,
    };
    let mapped = map_vm(&MapInput {
        rules: &m.spec.rules,
        item_id: item.id,
        item: &item.spec,
        vm,
        networks,
        format: &format,
    });
    let mut problems = Vec::new();
    let mut node = None;
    let mut node_name = None;
    let mut preview = None;
    match mapped {
        Err(found) => {
            // 매핑을 풀지 못해도 대상 폴더 권한은 먼저 본다(만들기와 같은 차례).
            if let Err(e) = state.authz.require_new(
                principal,
                Privilege::VmCreate,
                Kind::Vm,
                target_folder(&m.spec.rules, &item.spec),
            ) {
                if !e.status.is_client_error() {
                    return Err(e);
                }
                problems.push(plan::problem(
                    "folder",
                    VmCheckLevel::Error,
                    e.code,
                    e.message,
                ));
            } else {
                problems.extend(found);
            }
        }
        Ok(plan::Mapped {
            spec: mapped,
            volumes,
            warnings,
        }) => {
            let mut spec = serde_json::to_value(&mapped).map_err(ApiError::internal)?;
            // 머신은 클러스터 노드에 맞춰 채운다(만들기 `machine`을 뺀 것과 같다).
            if let Some(o) = spec.as_object_mut() {
                o.remove("machine");
            }
            let req = VmCheckRequest {
                name: item.spec.target_name.clone(),
                labels: BTreeMap::new(),
                spec,
                volumes,
                rules: Vec::new(),
            };
            match Box::pin(check_planned(state, principal, req)).await {
                Ok(c) => {
                    let mut spec = mapped;
                    if let Some(machine) = c.check.machine {
                        spec.machine = machine;
                    }
                    let refused = c
                        .check
                        .problems
                        .iter()
                        .any(|p| p.code == "forbidden" || p.code == "not_found");
                    let ok = c.check.ok;
                    if ok {
                        node = c.node.or(spec.node);
                        node_name = c.check.node.clone();
                    }
                    problems.extend(c.check.problems);
                    if !refused {
                        problems.extend(warnings);
                        let overridden = item
                            .spec
                            .overrides
                            .as_ref()
                            .is_some_and(|o| o.folder.is_some());
                        if let (VmwareFolderRule::Mirror { .. }, false, Some(path)) =
                            (&m.spec.rules.folders, overridden, plan::mirror_path(vm))
                        {
                            problems.push(plan::problem(
                                "folder",
                                VmCheckLevel::Warn,
                                "folder_created",
                                format!("VM folder {path:?} will be created for this VM"),
                            ));
                        }
                        problems.extend(guest_hints(&m.spec.rules, vm, &spec));
                    }
                    preview = ok.then_some(spec);
                }
                Err(e) if e.status.is_client_error() => {
                    // 대상 폴더에 `vm.create`가 없다(볼 수 없으면 404) — 그 밖을 알려 주지 않는다.
                    problems.push(plan::problem(
                        "folder",
                        VmCheckLevel::Error,
                        e.code,
                        e.message,
                    ));
                }
                Err(e) => return Err(e),
            }
        }
    }
    let ours: Vec<&str> = [&item.status.snapshot, &item.status.pending_snapshot]
        .into_iter()
        .flatten()
        .map(|s| s.moref.as_str())
        .collect();
    let ctx = plan::SourceContext {
        mode: m.spec.mode,
        cutover: &m.spec.cutover,
        status: &source.status,
        transport: &source.spec.transport,
        inventory: inv,
    };
    problems.extend(source_problems(&ctx, &item.spec, vm, &ours));
    Ok(Checked {
        vm: PreflightVm {
            item: item.name.clone(),
            source_ref: vm.moref.clone(),
            name: vm.name.clone(),
            level: level_of(&problems),
            problems,
            node: node_name,
            spec_preview: preview,
        },
        node,
    })
}

/// 고른 항목들을 미리 확인한다.
async fn preflight_items(
    state: &AppState,
    principal: &Principal,
    m: &VmwareMigration,
    items: &[VmwareVm],
    refresh: bool,
) -> Result<(Vec<Checked>, u64), ApiError> {
    let source = source_of(state, &m.spec)?;
    let (inv, fetched_at) = inventory_of(state, &source, refresh).await?;
    let networks = visible_networks(state, principal)?;
    let mut out = Vec::new();
    for item in items {
        out.push(check_item(state, principal, m, &source, &inv, &networks, item).await?);
    }
    Ok((out, fetched_at))
}

/// 미리 확인한 항목들을 `pending`(통과 — 고른 노드를 적음) 또는 `failed`(막힘 — 이유)로 둔다.
async fn admit(state: &AppState, items: &[VmwareVm], checked: &[Checked]) -> Result<(), ApiError> {
    let at = now();
    for (item, c) in items.iter().zip(checked) {
        let mut status = item.status.clone();
        let errors: Vec<String> =
            c.vm.problems
                .iter()
                .filter(|p| p.level == VmCheckLevel::Error)
                .map(|p| format!("{}: {}", p.code, p.message))
                .collect();
        status.warnings =
            c.vm.problems
                .iter()
                .filter(|p| p.level == VmCheckLevel::Warn)
                .map(|p| p.message.clone())
                .collect();
        if errors.is_empty() {
            status.state = VmwareVmState::Pending;
            status.error = None;
            status.push_history(at, "preflight passed");
            if item.spec.node.is_none()
                && let Some(node) = c.node
            {
                let spec = VmwareVmSpec {
                    node: Some(node),
                    ..item.spec.clone()
                };
                state
                    .cluster
                    .update_spec::<VmwareVmSpec>(item.id, None, item.labels.clone(), &spec)
                    .await?;
            }
        } else {
            status.state = VmwareVmState::Failed;
            status.error = Some(format!("preflight: {}", errors.join("; ")));
            status.push_history(at, "preflight blocked");
        }
        state
            .cluster
            .update_status::<VmwareVmSpec>(item.id, &status)
            .await?;
    }
    Ok(())
}

// ---- 핸들러 ----

/// 이전 계획을 만든다(201 `draft`, 항목마다 `vmware-vm`): 원본 연결의 인벤토리(노드 캐시)에서 VM을 찾고(없으면 400
/// `vmware_vm_not_found`), 규칙을 검증한다(400 `invalid_vmware_migration`). 같은 VM이 끝나지 않은 다른 계획에 있으면 409
/// `vm_in_migration`. 규칙·덮어쓰기의 폴더는 그 폴더의 `vm.create`(Mirror는 `inventory.folder`)가 든다. 모든 노드가
/// `vmware-migrate`를 알아야 한다(409 `cluster_upgrading`). 아직 아무것도 복사하지 않는다 — 미리 확인하고 시작한다.
#[utoipa::path(post, path = "/vmware-migrations", tag = "vmware",
    request_body = CreateMigrationRequest,
    responses((status = 201, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateMigrationRequest>,
) -> Result<(StatusCode, Json<MigrationView>), ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    check_name(&req.name)?;
    if req.vms.is_empty() || req.vms.len() > MAX_MIGRATION_VMS {
        return Err(invalid(format!(
            "a migration moves 1-{MAX_MIGRATION_VMS} VMs; split larger waves into several migrations"
        )));
    }
    let source = lookup::<VmwareSourceSpec>(&state.cluster, &req.source)
        .map_err(|e| ApiError::bad_request(e.message).with_code("source_not_found"))?;
    let spec = VmwareMigrationSpec {
        source: source.id,
        mode: req.mode,
        rules: req.rules,
        limits: req.limits,
        sync: req.sync,
        cutover: req.cutover,
        folder: None,
    };
    spec.validate().map_err(invalid)?;
    check_folders(&state, &principal, &spec.rules, &req.vms)?;
    audit.detail(format!(
        "vmware-migration create {} ({}, {} VMs)",
        req.name,
        mode_name(spec.mode),
        req.vms.len()
    ));
    let (inv, _) = inventory_of(&state, &source, false).await?;
    let mut migration: VmwareMigration = Resource::new(req.name, spec);
    migration.labels = req.labels;
    let items = build_items(&state, &migration, &inv, &[], &req.vms)?;
    migration.status.counts = counts(&items);
    state.cluster.create(&migration).await?;
    if let Err(e) = create_items(&state, &items).await {
        if let Err(d) = state
            .cluster
            .delete::<VmwareMigrationSpec>(migration.id, None)
            .await
        {
            tracing::warn!(migration = %migration.name, error = %d, "cannot remove a migration without items");
        }
        return Err(e);
    }
    Ok((StatusCode::CREATED, Json(view(&state, migration.id)?)))
}

/// 계획을 바꾼다(`draft`·`paused`만 — 아니면 409 `migration_running`): 규칙·한도·주기·전환 정책, VM 더하기·빼기. 시작한
/// VM은 뺄 수 없다(409 `vm_started` — 취소한다). 멈춘 계획에 더한 VM은 이어 갈 때 미리 확인한다. `If-Match`로 revision을 볼 수
/// 있다.
#[utoipa::path(patch, path = "/vmware-migrations/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = MigrationPatch,
    responses((status = 200, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn patch_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<MigrationPatch>,
) -> Result<Json<MigrationView>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-migration update {}", m.name));
    if !matches!(
        m.status.phase,
        VmwareMigrationPhase::Draft | VmwareMigrationPhase::Paused
    ) {
        return Err(ApiError::conflict(
            "migration_running",
            format!(
                "vmware-migration {:?} can be changed only while draft or paused",
                m.name
            ),
        ));
    }
    let mut spec = m.spec.clone();
    if let Some(r) = patch.rules {
        spec.rules = r;
    }
    if let Some(l) = patch.limits {
        spec.limits = l;
    }
    if let Some(s) = patch.sync {
        spec.sync = s;
    }
    if let Some(c) = patch.cutover {
        spec.cutover = c;
    }
    spec.validate().map_err(invalid)?;
    check_folders(&state, &principal, &spec.rules, &patch.add_vms)?;
    let items = items_of(&state, m.id)?;
    let mut remove = Vec::new();
    for key in &patch.remove_vms {
        let item = find_item(&items, key).ok_or_else(|| {
            ApiError::bad_request(format!("{key:?} is not in vmware-migration {:?}", m.name))
                .with_code("vmware_vm_not_found")
        })?;
        if !not_started(&item.status) {
            return Err(ApiError::conflict(
                "vm_started",
                format!(
                    "{} has started; cancel it instead of removing it",
                    item.name
                ),
            ));
        }
        remove.push(item.clone());
    }
    let added = if patch.add_vms.is_empty() {
        Vec::new()
    } else {
        let source = source_of(&state, &spec)?;
        let (inv, _) = inventory_of(&state, &source, false).await?;
        let kept: Vec<VmwareVm> = items
            .iter()
            .filter(|i| !remove.iter().any(|r| r.id == i.id))
            .cloned()
            .collect();
        build_items(&state, &m, &inv, &kept, &patch.add_vms)?
    };
    if items.len() - remove.len() + added.len() > MAX_MIGRATION_VMS {
        return Err(invalid(format!(
            "a migration moves at most {MAX_MIGRATION_VMS} VMs"
        )));
    }
    if items.len() == remove.len() && added.is_empty() {
        return Err(invalid("a migration needs at least one VM"));
    }
    let expected = if_match(&headers)?.unwrap_or(m.revision);
    state
        .cluster
        .update_spec::<VmwareMigrationSpec>(m.id, Some(expected), m.labels.clone(), &spec)
        .await?;
    create_items(&state, &added).await?;
    for item in &remove {
        state
            .cluster
            .delete::<VmwareVmSpec>(item.id, Some(item.revision))
            .await?;
    }
    recount(&state, m.id).await?;
    Ok(Json(view(&state, m.id)?))
}

/// 계획과 항목을 지운다(204) — 만든 VM·볼륨·원본 VM은 그대로다. `draft`이거나 끝난(`finished`·`cancelled`) 계획이고 항목이
/// 모두 끝났거나 시작하지 않았을 때만(아니면 409 `migration_active` — 먼저 취소한다).
#[utoipa::path(delete, path = "/vmware-migrations/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_migration(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-migration delete {}", m.name));
    let items = items_of(&state, m.id)?;
    let busy = items
        .iter()
        .find(|i| !(i.status.state.is_final() || not_started(&i.status)));
    let deletable = m.status.phase == VmwareMigrationPhase::Draft || m.status.phase.is_done();
    if !deletable || busy.is_some() {
        return Err(ApiError::conflict(
            "migration_active",
            format!(
                "vmware-migration {:?} is {}{}; cancel it first",
                m.name,
                serde_json::to_value(m.status.phase)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                busy.map(|i| format!(" and {} is {}", i.name, i.status.state.as_str()))
                    .unwrap_or_default()
            ),
        ));
    }
    for item in &items {
        state
            .cluster
            .delete::<VmwareVmSpec>(item.id, Some(item.revision))
            .await?;
    }
    state
        .cluster
        .delete::<VmwareMigrationSpec>(m.id, Some(m.revision))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// VM마다 미리 확인한다(200 — 문제가 있어도): 계획 71 `vm-checks`의 만들기 검사(대상 폴더 권한 먼저, 첫 거절에서 멈춤),
/// 만들 볼륨(디스크마다 — 크기 = 원본 용량, 데이터스토어 규칙의 풀), 할당량·이름·MAC·노드, 그리고 VMware 쪽(사용자 스냅샷·
/// 독립 디스크·RDM·vSAN·FT·템플릿·하드웨어 버전·무료 ESXi·전원·Tools·멀티라이터)과 게스트 힌트. 아무것도 쓰지 않는다.
#[utoipa::path(post, path = "/vmware-migrations/{key}/preflight", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    request_body = PreflightRequest,
    responses((status = 200, body = PreflightAnswer), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn preflight_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    req: Option<Json<PreflightRequest>>,
) -> Result<Json<PreflightAnswer>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let req = req.map(|Json(r)| r).unwrap_or_default();
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    let all = items_of(&state, m.id)?;
    let items: Vec<VmwareVm> = if req.vms.is_empty() {
        all
    } else {
        req.vms
            .iter()
            .map(|k| {
                find_item(&all, k).cloned().ok_or_else(|| {
                    ApiError::bad_request(format!("{k:?} is not in vmware-migration {:?}", m.name))
                        .with_code("vmware_vm_not_found")
                })
            })
            .collect::<Result<_, _>>()?
    };
    let (checked, fetched_at) =
        Box::pin(preflight_items(&state, &principal, &m, &items, req.refresh)).await?;
    Ok(Json(PreflightAnswer {
        fetched_at,
        vms: checked.into_iter().map(|c| c.vm).collect(),
    }))
}

/// 계획을 시작한다(`draft` → `running`, 아니면 409 `migration_not_draft`): 인벤토리를 다시 읽어 미리 확인하고, 막힌 VM은
/// `failed`(이유 — 나머지는 간다), 통과한 VM은 `pending`(고른 노드)로 둔다. 복사는 스케줄러가 한다(스테이지 4).
#[utoipa::path(post, path = "/vmware-migrations/{key}/actions/start", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn start_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<MigrationView>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    let items = items_of(&state, m.id)?;
    audit.detail(format!(
        "vmware-migration start {} ({}, {} VMs)",
        m.name,
        mode_name(m.spec.mode),
        items.len()
    ));
    if m.status.phase != VmwareMigrationPhase::Draft {
        return Err(ApiError::conflict(
            "migration_not_draft",
            format!("vmware-migration {:?} has already started", m.name),
        ));
    }
    Box::pin(admit_drafts(
        &state,
        &principal,
        &m,
        &items,
        VmwareMigrationPhase::Running,
    ))
    .await?;
    Ok(Json(view(&state, m.id)?))
}

/// `draft` 항목을 미리 확인해 받아들이고 계획을 `phase`로 둔다(먼저 계획을 바꿔 두 요청이 함께 받아들이지 않게).
async fn admit_drafts(
    state: &AppState,
    principal: &Principal,
    m: &VmwareMigration,
    items: &[VmwareVm],
    phase: VmwareMigrationPhase,
) -> Result<(), ApiError> {
    let drafts: Vec<VmwareVm> = items
        .iter()
        .filter(|i| i.status.state == VmwareVmState::Draft)
        .cloned()
        .collect();
    let (checked, _) = Box::pin(preflight_items(state, principal, m, &drafts, true)).await?;
    write_status(
        state,
        m,
        &VmwareMigrationStatus {
            phase,
            error: None,
            ..m.status.clone()
        },
    )
    .await?;
    admit(state, &drafts, &checked).await?;
    recount(state, m.id).await
}

async fn set_phase(
    state: &AppState,
    m: &VmwareMigration,
    from: VmwareMigrationPhase,
    to: VmwareMigrationPhase,
) -> Result<(), ApiError> {
    if m.status.phase != from {
        return Err(ApiError::conflict(
            "migration_state",
            format!(
                "vmware-migration {:?} is {}, not {}",
                m.name,
                phase_name(m.status.phase),
                phase_name(from)
            ),
        ));
    }
    write_status(
        state,
        m,
        &VmwareMigrationStatus {
            phase: to,
            ..m.status.clone()
        },
    )
    .await
}

fn phase_name(p: VmwareMigrationPhase) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// 계획을 멈춘다(`running` → `paused`, 아니면 409 `migration_state`): 스케줄러가 새 동기화·전환을 만들지 않는다(도는 Task는
/// 끝까지 간다).
#[utoipa::path(post, path = "/vmware-migrations/{key}/actions/pause", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn pause_migration(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<MigrationView>, ApiError> {
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-migration pause {}", m.name));
    set_phase(
        &state,
        &m,
        VmwareMigrationPhase::Running,
        VmwareMigrationPhase::Paused,
    )
    .await?;
    Ok(Json(view(&state, m.id)?))
}

/// 멈춘 계획을 잇는다(`paused` → `running`, 아니면 409 `migration_state`). 멈춘 사이 더한 VM은 미리 확인해 받아들인다.
#[utoipa::path(post, path = "/vmware-migrations/{key}/actions/resume", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn resume_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<MigrationView>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-migration resume {}", m.name));
    if m.status.phase != VmwareMigrationPhase::Paused {
        return Err(ApiError::conflict(
            "migration_state",
            format!(
                "vmware-migration {:?} is {}, not paused",
                m.name,
                phase_name(m.status.phase)
            ),
        ));
    }
    let items = items_of(&state, m.id)?;
    if items.iter().any(|i| i.status.state == VmwareVmState::Draft) {
        Box::pin(admit_drafts(
            &state,
            &principal,
            &m,
            &items,
            VmwareMigrationPhase::Running,
        ))
        .await?;
    } else {
        set_phase(
            &state,
            &m,
            VmwareMigrationPhase::Paused,
            VmwareMigrationPhase::Running,
        )
        .await?;
    }
    Ok(Json(view(&state, m.id)?))
}

/// 계획을 취소한다(`cancelled`, 끝난 계획은 409 `migration_state`). 원본·우리 쪽에 남긴 것이 없는 항목은 곧 `cancelled`다.
/// 복사를 시작한 항목은 스케줄러가 만드는 정리 Task(우리 스냅샷·대상 볼륨 지움 — 원본은 그대로)가 맡는다. 전환한
/// (`done`) 항목과 전환 중인 항목은 건드리지 않는다 — 마무리나 되돌리기로 끝낸다(전환 중이던 항목이 되돌려지면 그때
/// 정리한다). 원본 전원을 되돌릴 수 있는 항목(전환 중·되돌림 실패)이 있으면 그 대상 폴더의 `vm.create`·`vm.power`가 든다.
#[utoipa::path(post, path = "/vmware-migrations/{key}/actions/cancel", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = MigrationView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn cancel_migration(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<MigrationView>, ApiError> {
    let m = lookup::<VmwareMigrationSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-migration cancel {}", m.name));
    if m.status.phase.is_done() {
        return Err(ApiError::conflict(
            "migration_state",
            format!(
                "vmware-migration {:?} is already {}",
                m.name,
                phase_name(m.status.phase)
            ),
        ));
    }
    for item in items_of(&state, m.id)? {
        if item.status.state != VmwareVmState::Done
            && !item.status.state.is_final()
            && touches_power(&item.status)
        {
            require_item_power(&state, &principal, &m, &item)?;
        }
    }
    write_status(
        &state,
        &m,
        &VmwareMigrationStatus {
            phase: VmwareMigrationPhase::Cancelled,
            ..m.status.clone()
        },
    )
    .await?;
    let at = now();
    for item in items_of(&state, m.id)? {
        if not_started(&item.status) {
            let mut status = item.status.clone();
            status.state = VmwareVmState::Cancelled;
            status.push_history(at, "cancelled");
            state
                .cluster
                .update_status::<VmwareVmSpec>(item.id, &status)
                .await?;
        }
    }
    recount(&state, m.id).await?;
    Ok(Json(view(&state, m.id)?))
}

/// 항목 동작(`retry`·`cancel`·`cutover`·`revert`·`finalize`)의 답: 바뀐 항목과 만든 Task
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemAction {
    pub item: Resource<VmwareVmSpec, VmwareVmStatus>,
    /// 만든 Task(취소·마무리의 `vmware-cleanup`, 전환 `vmware-cutover`, 되돌리기 `vmware-revert`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub task: Option<Uuid>,
}

fn item_state_conflict(item: &VmwareVm, what: &str) -> ApiError {
    ApiError::conflict(
        "vmware_vm_state",
        format!(
            "vmware-vm {:?} is {}; {what}",
            item.name,
            item.status.state.as_str()
        ),
    )
}

/// 항목의 끝나지 않은 VMware Task가 있나
fn item_busy(state: &AppState, item: Uuid) -> Result<bool, ApiError> {
    let (_, tasks) = state.cluster.list::<opl_types::task::TaskSpec>()?;
    Ok(tasks.iter().any(|t| {
        t.spec.target == item
            && !t.status.state.is_finished()
            && matches!(
                t.spec.op,
                opl_types::task::TaskOp::VmwareSync { .. }
                    | opl_types::task::TaskOp::VmwareCutover
                    | opl_types::task::TaskOp::VmwareRevert
                    | opl_types::task::TaskOp::VmwareCleanup { .. }
            )
    }))
}

/// 실패한 항목을 다시 한다(계획이 `running`·`paused`일 때, 아니면 409 `migration_state`). 복사를 시작하지 않은 항목(미리
/// 확인에서 막힘)은 미리 확인을 다시 돌려 `pending` 또는 `failed`로, 복사를 시작한 항목은 체크포인트부터 잇게
/// (`copying`·`syncing`·`ready`) 둔다 — 스케줄러가 다음 틱에 동기화를 만든다. 오래 전환하지 않아 멈춘 따뜻한 항목(`syncing`,
/// 다음 동기화 없음)은 다시 시작한다. 전환의 자동 되돌림이 실패한 항목(`failed` + 걸음)은 되돌림을 다시 하는 `vmware-cutover`
/// Task를 만든다 — 대상 폴더의 `vm.create`·`vm.power`가 들고, 이미 다시 하기를 요청했으면 409 `migration_busy`다. 그 밖은
/// 409 `vmware_vm_state`.
#[utoipa::path(post, path = "/vmware-vms/{key}/retry", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = ItemAction), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn retry_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<ItemAction>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &key)?;
    audit.detail(format!("vmware-vm retry {}", item.name));
    let m = state
        .cluster
        .get::<VmwareMigrationSpec>(item.spec.migration)?
        .ok_or_else(|| ApiError::not_found("vmware-migration not found"))?;
    if !matches!(
        m.status.phase,
        VmwareMigrationPhase::Running | VmwareMigrationPhase::Paused
    ) {
        return Err(ApiError::conflict(
            "migration_state",
            format!(
                "vmware-migration {:?} is {}, not running or paused",
                m.name,
                phase_name(m.status.phase)
            ),
        ));
    }
    if item_busy(&state, item.id)? {
        return Err(ApiError::conflict(
            "migration_busy",
            format!(
                "vmware-vm {:?} has a task running; try again later",
                item.name
            ),
        ));
    }
    // 전환의 자동 되돌림이 실패한 항목: 되돌림을 다시 한다(전환 Task가 적힌 걸음부터 되돌린다). 원본·대상 VM 전원을
    // 다루므로 전환과 같은 권한이고, 겹친 요청이 Task를 둘 만들지 않게 만들 Task의 id를 revision을 보고 먼저 적는다
    // (`rollback_task` — 전환 Task는 그 id일 때만 되돌리고, 그 Task가 항목을 다른 상태에서 보면 아무것도 하지 않는다).
    if item.status.state == VmwareVmState::Failed && item.status.cutover_step.is_some() {
        require_item_power(&state, &principal, &m, &item)?;
        let id = Uuid::now_v7();
        let mut status = item.status.clone();
        status.rollback_task = Some(id);
        status.push_history(now(), "rollback retry requested");
        write_item(&state, &item, &status).await?;
        let task = crate::tasks::create_task_with_id(
            &state.cluster,
            id,
            opl_types::task::TaskOp::VmwareCutover,
            item.id,
            item_node(&state, &item),
        )
        .await?;
        let item = lookup::<VmwareVmSpec>(&state.cluster, &item.id.to_string())?;
        return Ok(Json(ItemAction {
            item,
            task: Some(task.id),
        }));
    }
    let at = now();
    let stopped = item.status.state == VmwareVmState::Syncing && item.status.next_sync_at.is_none();
    if item.status.state == VmwareVmState::Failed && not_started(&item.status) {
        let mut again = item.clone();
        again.status.sync_failures = 0;
        let (checked, _) = Box::pin(preflight_items(
            &state,
            &principal,
            &m,
            std::slice::from_ref(&again),
            true,
        ))
        .await?;
        admit(&state, std::slice::from_ref(&again), &checked).await?;
    } else if item.status.state == VmwareVmState::Failed || stopped {
        let mut status = item.status.clone();
        let warm = m.spec.mode == VmwareMigrationMode::Warm;
        status.state = if item.status.rounds == 0 {
            VmwareVmState::Copying
        } else if warm {
            VmwareVmState::Syncing
        } else {
            VmwareVmState::Ready
        };
        status.next_sync_at = (warm || item.status.rounds == 0).then_some(at);
        if stopped {
            // 멈춘 날부터 다시 센다.
            status.started_at = Some(at);
        }
        status.error = None;
        status.sync_failures = 0;
        status.push_history(at, "retry requested");
        state
            .cluster
            .update_status_checked::<VmwareVmSpec>(item.id, item.revision, &status)
            .await
            .map_err(|e| match e {
                ClusterError::Rejected(Rejection::RevisionMismatch { .. }) => ApiError::conflict(
                    "migration_busy",
                    format!(
                        "vmware-vm {:?} was changed by another request; try again",
                        item.name
                    ),
                ),
                e => e.into(),
            })?;
    } else {
        return Err(item_state_conflict(
            &item,
            "only failed or stopped VMs can be retried",
        ));
    }
    recount(&state, m.id).await?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &item.id.to_string())?;
    Ok(Json(ItemAction { item, task: None }))
}

/// 항목 하나를 취소한다(설계 §10.3): 원본·우리 쪽에 남긴 것이 없으면 곧 `cancelled`, 있으면 `vmware-cleanup` Task(우리
/// 스냅샷 지움, 우리가 켠 CBT 끔, 대상 볼륨 지움 — 원본 VM·디스크는 그대로)를 항목의 노드에 만든다(도는 동기화 뒤에 돈다).
/// 끝났거나 전환 중·전환한 항목은 409 `vmware_vm_state`. 원본 전원을 되돌릴 수 있는 항목(전환의 되돌림이 실패함)은 대상
/// 폴더의 `vm.create`·`vm.power`가 든다.
#[utoipa::path(post, path = "/vmware-vms/{key}/cancel", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = ItemAction), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn cancel_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<ItemAction>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &key)?;
    if touches_power(&item.status) {
        require_item_power(&state, &principal, &migration_of(&state, &item)?, &item)?;
    }
    audit.detail(format!("vmware-vm cancel {}", item.name));
    if item.status.state.is_final()
        || matches!(
            item.status.state,
            VmwareVmState::CuttingOver | VmwareVmState::Done
        )
    {
        return Err(item_state_conflict(
            &item,
            "only VMs that have not been cut over can be cancelled",
        ));
    }
    if holds_nothing(&item.status) && !item_busy(&state, item.id)? {
        let mut status = item.status.clone();
        status.state = VmwareVmState::Cancelled;
        status.next_sync_at = None;
        status.push_history(now(), "cancelled");
        state
            .cluster
            .update_status::<VmwareVmSpec>(item.id, &status)
            .await?;
        recount(&state, item.spec.migration).await?;
        let item = lookup::<VmwareVmSpec>(&state.cluster, &item.id.to_string())?;
        return Ok(Json(ItemAction { item, task: None }));
    }
    // 대상 볼륨은 항목의 노드(로컬 풀일 수 있다)에 있다. 그 노드가 오프라인이면 이 노드가 원본 쪽만이라도 치운다.
    let online = |id: Uuid| {
        state
            .cluster
            .get::<opl_types::node::NodeSpec>(id)
            .ok()
            .flatten()
            .is_some_and(|n| n.status.online)
    };
    let node = item.spec.node.filter(|n| online(*n)).unwrap_or(state.node);
    let task = crate::tasks::create_task(
        &state.cluster,
        opl_types::task::TaskOp::VmwareCleanup {
            delete_target: true,
        },
        item.id,
        node,
    )
    .await?;
    Ok(Json(ItemAction {
        item,
        task: Some(task.id),
    }))
}

/// 항목을 치우면 원본 전원이나 대상 VM을 건드리나(전환을 시작했다 — 되돌림이 실패한 항목 등): 그러면 전환·되돌리기와 같은
/// 권한이 든다.
fn touches_power(s: &VmwareVmStatus) -> bool {
    s.state == VmwareVmState::CuttingOver
        || s.cutover_step.is_some()
        || s.target_vm.is_some()
        || s.source_changes.powered_off_by_us
}

/// 항목의 계획(없으면 404)
fn migration_of(state: &AppState, item: &VmwareVm) -> Result<VmwareMigration, ApiError> {
    state
        .cluster
        .get::<VmwareMigrationSpec>(item.spec.migration)?
        .ok_or_else(|| ApiError::not_found("vmware-migration not found"))
}

/// 전환·되돌리기·마무리의 권한(설계 §13): 항목을 볼 수 있고(루트 `system.read`) 대상 폴더에 `vm.create`와 `vm.power`
fn require_item_power(
    state: &AppState,
    principal: &Principal,
    m: &VmwareMigration,
    item: &VmwareVm,
) -> Result<(), ApiError> {
    state.authorize(principal, Action::Read, VmwareVmSpec::KIND)?;
    let folder = target_folder(&m.spec.rules, &item.spec);
    for p in [Privilege::VmCreate, Privilege::VmPower] {
        state.authz.require_new(principal, p, Kind::Vm, folder)?;
    }
    Ok(())
}

fn require_busy_free(state: &AppState, item: &VmwareVm) -> Result<(), ApiError> {
    if item_busy(state, item.id)? {
        return Err(ApiError::conflict(
            "migration_busy",
            format!(
                "vmware-vm {:?} has a task running; try again later",
                item.name
            ),
        ));
    }
    Ok(())
}

/// 항목 Task를 둘 노드: 항목의 노드(대상 볼륨이 있는 곳)가 온라인이면 그곳, 아니면 이 노드
fn item_node(state: &AppState, item: &VmwareVm) -> Uuid {
    item.spec
        .node
        .filter(|n| {
            state
                .cluster
                .get::<opl_types::node::NodeSpec>(*n)
                .ok()
                .flatten()
                .is_some_and(|n| n.status.online)
        })
        .unwrap_or(state.node)
}

/// 항목 상태를 `revision`일 때만 바꾼다(겹친 요청은 409 `migration_busy`).
async fn write_item(
    state: &AppState,
    item: &VmwareVm,
    status: &VmwareVmStatus,
) -> Result<(), ApiError> {
    state
        .cluster
        .update_status_checked::<VmwareVmSpec>(item.id, item.revision, status)
        .await
        .map(|_| ())
        .map_err(|e| match e {
            ClusterError::Rejected(Rejection::RevisionMismatch { .. }) => ApiError::conflict(
                "migration_busy",
                format!(
                    "vmware-vm {:?} was changed by another request; try again",
                    item.name
                ),
            ),
            e => e.into(),
        })
}

/// VM 하나를 전환한다(설계 §10.1): `ready`·`syncing`·`rolled-back` 항목만(아니면 409 `not_ready`), 계획이 `running`·`paused`일
/// 때(409 `migration_state`). 원본을 건드리기 전에 다시 본다 — 대상 이름이 그새 쓰였으면 409 `name_taken`, VMware 쪽 표(노드
/// 캐시의 인벤토리 — 사용자 스냅샷·FT·Tools 등)에 막는 문제가 있으면 409 `preflight_blocked`(`details.problems`). 할당량·용량은
/// VM을 만들 때 상태 머신이 보고, 넘으면 자동 되돌림이다. Task `vmware-cutover`를 항목의 노드에 만든다. 대상 폴더의
/// `vm.create`·`vm.power`가 든다.
#[utoipa::path(post, path = "/vmware-vms/{key}/cutover", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = ItemAction), (status = "4XX", body = crate::error::ErrorBody),
        (status = 502, body = crate::error::ErrorBody)))]
pub async fn cutover_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<ItemAction>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &key)?;
    let m = migration_of(&state, &item)?;
    require_item_power(&state, &principal, &m, &item)?;
    audit.detail(format!("vmware-vm cutover {}", item.name));
    if !matches!(
        m.status.phase,
        VmwareMigrationPhase::Running | VmwareMigrationPhase::Paused
    ) {
        return Err(ApiError::conflict(
            "migration_state",
            format!(
                "vmware-migration {:?} is {}, not running or paused",
                m.name,
                phase_name(m.status.phase)
            ),
        ));
    }
    if !matches!(
        item.status.state,
        VmwareVmState::Ready | VmwareVmState::Syncing | VmwareVmState::RolledBack
    ) {
        return Err(ApiError::conflict(
            "not_ready",
            format!(
                "vmware-vm {:?} is {}; only ready or syncing VMs can be cut over",
                item.name,
                item.status.state.as_str()
            ),
        ));
    }
    require_busy_free(&state, &item)?;
    if let Some(vm) = state
        .cluster
        .get_by_name::<VmSpec>(&item.spec.target_name)?
        && Some(vm.id) != item.status.target_vm
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("a VM named {:?} already exists", item.spec.target_name),
        ));
    }
    // 원본 VM 하나를 새로 읽는다(캐시의 인벤토리는 60초까지 묵는다 — 그새 생긴 사용자 스냅샷 등).
    let source = source_of(&state, &m.spec)?;
    let (inv, _) = inventory_of(&state, &source, false).await?;
    let session = state
        .vmware_cache
        .session(&source)
        .await
        .map_err(VsphereError::api_error)?;
    let fresh =
        match crate::vmware::inventory::refresh_vm(&session, &inv, &item.spec.source_ref).await {
            Ok(vm) => Some(vm),
            Err(VsphereError::NotFound { .. }) => None,
            Err(e) => return Err(e.api_error()),
        };
    if let Some(vm) = &fresh {
        let ours: Vec<&str> = [&item.status.snapshot, &item.status.pending_snapshot]
            .into_iter()
            .flatten()
            .map(|s| s.moref.as_str())
            .collect();
        let ctx = plan::SourceContext {
            mode: m.spec.mode,
            cutover: &m.spec.cutover,
            status: &source.status,
            transport: &source.spec.transport,
            inventory: &inv,
        };
        let problems: Vec<VmCheckProblem> = source_problems(&ctx, &item.spec, vm, &ours)
            .into_iter()
            .filter(|p| p.level == VmCheckLevel::Error)
            .collect();
        if !problems.is_empty() {
            let text: Vec<String> = problems
                .iter()
                .map(|p| format!("{}: {}", p.code, p.message))
                .collect();
            let mut e = ApiError::conflict(
                "preflight_blocked",
                format!(
                    "vmware-vm {:?} cannot be cut over: {}",
                    item.name,
                    text.join("; ")
                ),
            );
            e.details = Some(serde_json::json!({ "problems": problems }));
            return Err(e);
        }
    }
    let mut status = item.status.clone();
    status.cutover_requested_at = Some(now());
    write_item(&state, &item, &status).await?;
    let task = crate::tasks::create_task(
        &state.cluster,
        opl_types::task::TaskOp::VmwareCutover,
        item.id,
        item_node(&state, &item),
    )
    .await?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &item.id.to_string())?;
    Ok(Json(ItemAction {
        item,
        task: Some(task.id),
    }))
}

/// 켠 뒤 되돌리기의 확인
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RevertRequest {
    /// 대상(Orbivisor) VM 이름. 되돌리면 그 VM에서 쓴 데이터는 원본으로 가지 않는다.
    pub confirm: String,
}

/// 전환한 VM을 되돌린다(설계 §10.3, `done`만 — 아니면 409 `vmware_vm_state`): 대상 VM을 끄고(지우지 않음) 원본 NIC·설명을
/// 되돌리고 원본을 켠다 → `reverted`. 대상에서 쓴 데이터는 원본으로 가지 않으므로 본문 `confirm`이 대상 VM 이름이어야 한다
/// (아니면 400 `confirm_mismatch`). 대상 폴더의 `vm.create`·`vm.power`가 든다.
#[utoipa::path(post, path = "/vmware-vms/{key}/revert", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    request_body = RevertRequest,
    responses((status = 200, body = ItemAction), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn revert_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<RevertRequest>,
) -> Result<Json<ItemAction>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &key)?;
    let m = migration_of(&state, &item)?;
    require_item_power(&state, &principal, &m, &item)?;
    audit.detail(format!("vmware-vm revert {}", item.name));
    if item.status.state != VmwareVmState::Done {
        return Err(item_state_conflict(
            &item,
            "only cut over VMs can be reverted",
        ));
    }
    let target = item
        .status
        .target_vm
        .and_then(|id| state.cluster.get::<VmSpec>(id).ok().flatten())
        .map_or_else(|| item.spec.target_name.clone(), |vm| vm.name);
    if req.confirm != target {
        return Err(ApiError::bad_request(format!(
            "type the Orbivisor VM name {target:?} to revert; writes made on it are not copied back to the source"
        ))
        .with_code("confirm_mismatch"));
    }
    require_busy_free(&state, &item)?;
    let task = crate::tasks::create_task(
        &state.cluster,
        opl_types::task::TaskOp::VmwareRevert,
        item.id,
        item_node(&state, &item),
    )
    .await?;
    Ok(Json(ItemAction {
        item,
        task: Some(task.id),
    }))
}

/// 전환을 마무리한다(설계 §10.3, `done`만 — 아니면 409 `vmware_vm_state`): `vmware-cleanup` — 원본에 남은 우리 스냅샷을 지우고
/// 우리가 켠 CBT를 끈다. 원본 보호(NIC `startConnected=false`·설명)는 그대로 둔다 → `finalized`(되돌릴 수 없다). 대상 폴더의
/// `vm.create`·`vm.power`가 든다.
#[utoipa::path(post, path = "/vmware-vms/{key}/finalize", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = ItemAction), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn finalize_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<Json<ItemAction>, ApiError> {
    require_feature(&state.cluster, VMWARE_MIGRATE)?;
    let item = lookup::<VmwareVmSpec>(&state.cluster, &key)?;
    let m = migration_of(&state, &item)?;
    require_item_power(&state, &principal, &m, &item)?;
    audit.detail(format!("vmware-vm finalize {}", item.name));
    if item.status.state != VmwareVmState::Done {
        return Err(item_state_conflict(
            &item,
            "only cut over VMs can be finalized",
        ));
    }
    require_busy_free(&state, &item)?;
    let task = crate::tasks::create_task(
        &state.cluster,
        opl_types::task::TaskOp::VmwareCleanup {
            delete_target: false,
        },
        item.id,
        item_node(&state, &item),
    )
    .await?;
    Ok(Json(ItemAction {
        item,
        task: Some(task.id),
    }))
}

/// 이전 항목(`vmware-vm`). `migration`을 주면 그 계획의 것만(없는 계획은 404).
#[utoipa::path(get, path = "/vmware-vms", tag = "vmware",
    params(ItemsQuery),
    responses((status = 200, body = List<Resource<VmwareVmSpec, VmwareVmStatus>>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_items(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<ItemsQuery>,
) -> Result<Json<List<VmwareVm>>, ApiError> {
    state.authorize(&principal, Action::Read, VmwareVmSpec::KIND)?;
    let only = match &q.migration {
        Some(key) => Some(lookup::<VmwareMigrationSpec>(&state.cluster, key)?.id),
        None => None,
    };
    let (revision, items) = state.cluster.list::<VmwareVmSpec>()?;
    let items = items
        .into_iter()
        .filter(|i| only.is_none_or(|m| i.spec.migration == m))
        .collect();
    Ok(Json(List { revision, items }))
}

#[utoipa::path(get, path = "/vmware-vms/{key}", tag = "vmware",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = Resource<VmwareVmSpec, VmwareVmStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_item(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<VmwareVm>, ApiError> {
    state.authorize(&principal, Action::Read, VmwareVmSpec::KIND)?;
    Ok(Json(lookup::<VmwareVmSpec>(&state.cluster, &key)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(state: VmwareVmState) -> VmwareVm {
        let mut i: VmwareVm = Resource::new(
            "w-web01",
            VmwareVmSpec {
                migration: Uuid::nil(),
                source_ref: "vm-1".into(),
                instance_uuid: "5003".into(),
                target_name: "web01".into(),
                overrides: None,
                node: None,
            },
        );
        i.status.state = state;
        i
    }

    #[test]
    fn counts_group_items_by_state() {
        let items = [
            item(VmwareVmState::Draft),
            item(VmwareVmState::Draft),
            item(VmwareVmState::Failed),
        ];
        assert_eq!(
            counts(&items),
            BTreeMap::from([("draft".into(), 2), ("failed".into(), 1)])
        );
    }

    #[test]
    fn only_items_that_hold_nothing_count_as_not_started() {
        assert!(not_started(&item(VmwareVmState::Pending).status));
        assert!(!not_started(&item(VmwareVmState::Copying).status));
        let mut failed = item(VmwareVmState::Failed);
        assert!(not_started(&failed.status));
        failed.status.cbt_enabled_by_us = true;
        assert!(!not_started(&failed.status));
    }

    #[test]
    fn levels_follow_the_worst_problem() {
        let p = |level| plan::problem("x", level, "c", String::new());
        assert_eq!(level_of(&[]), PreflightLevel::Ok);
        assert_eq!(level_of(&[p(VmCheckLevel::Warn)]), PreflightLevel::Warn);
        assert_eq!(
            level_of(&[p(VmCheckLevel::Warn), p(VmCheckLevel::Error)]),
            PreflightLevel::Blocked
        );
    }

    #[test]
    fn items_are_found_by_name_moref_or_instance_uuid() {
        let items = [item(VmwareVmState::Draft)];
        let mut items = items;
        items[0].status.source = Some(opl_types::vmware::VmwareSourceVm {
            vm: opl_types::vm_import::SourceVm {
                name: "web01".into(),
                guest_os: "rhel9_64Guest".into(),
                firmware: opl_types::vm::Firmware::Bios,
                vcpus: 1,
                cores_per_socket: 1,
                memory_mib: 1024,
                disks: vec![],
                nics: vec![],
            },
            host: None,
            tools: None,
            power: None,
            guest_ips: BTreeMap::new(),
        });
        for key in ["w-web01", "vm-1", "5003", "web01"] {
            assert!(find_item(&items, key).is_some(), "{key}");
        }
        assert!(find_item(&items, "nope").is_none());
    }
}
