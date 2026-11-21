//! 게스트 CPU 모델 API(계획 63 §8·§13): VM의 모델·플래그 검증과 별칭 고정, `GET /vms/{key}/cpu`(노드마다 켤 수
//! 있는지), `GET /nodes/{key}/cpu`·`POST /nodes/{key}/cpu/refresh`(Task `node-cpu-probe`), `GET /cpu/models|flags|baseline`,
//! 설정 `cpu`의 검증과 영향 받는 실행 중 VM 수.
//!
//! 모델·플래그의 존재는 이 아키텍처 노드들이 알린 카탈로그로 본다(모델 목록을 코드에 박지 않는다). 모든 노드가
//! `cpu-models`를 알릴 때만 받는다(409 `cluster_upgrading`, 검증보다 먼저).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_reconciler::cpu::{CpuContext, CpuRefusal, NodeCatalog};
use opl_types::authz::Privilege;
use opl_types::cpu::{
    self, Baseline, BaselineMode, CpuCatalogSpec, CpuError, CpuFlag, CpuModel, CpuSettings,
    NodeCpu, ResolvedCpu, X86Level,
};
use opl_types::node::{CPU_MODELS, Node, NodeSpec};
use opl_types::resource::Resource;
use opl_types::resource::ResourceSpec;
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::{Arch, CpuSpec, PendingChange, ValidationError, Vm, VmPhase, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::scheduler::{Inventory, cpu_receives, host_cpu_warning};
use crate::state::AppState;
use crate::tasks::create_task;

/// 노드 CPU 다시 읽기(Task `node-cpu-probe`). opld가 검사 루프의 탐침으로 구현한다 — 검사 루프가 다음 회차에 앞
/// 결과로 되돌리지 않게 같은 탐침이 읽는다.
#[async_trait]
pub trait CpuRefresher: Send + Sync {
    /// 이 노드의 QEMU에 CPU 모델·플래그를 다시 묻고 카탈로그와 `NodeStatus.cpu`를 쓴다. 탐침이 실패하면 그 까닭이다
    /// (앞 카탈로그는 남고 `NodeStatus.cpu.error`에 적힌다).
    async fn refresh(&self) -> Result<(), String>;
}

// ---------------------------------------------------------------------------------------------------------------
// 오류
// ---------------------------------------------------------------------------------------------------------------

/// CPU 모델·플래그 문법 오류의 400(§13 오류 코드)
pub(crate) fn cpu_error(e: CpuError) -> ApiError {
    let code = match &e {
        CpuError::ModelName(_) => "unknown_cpu_model",
        CpuError::Arch { .. } => "cpu_model_arch",
        CpuError::FlagName(_)
        | CpuError::FlagSign(_)
        | CpuError::TooManyFlags
        | CpuError::DuplicateFlag(_) => "invalid_cpu_flags",
        CpuError::PinnedModel(_) | CpuError::DuplicatePinned(_) => "invalid_request",
    };
    ApiError::bad_request(e.to_string()).with_code(code)
}

/// VM 스펙 검증 오류의 400. CPU 오류는 §13의 코드, NUMA·전용 CPU·hugepages 오류는 `invalid_numa`(계획 64)다.
pub(crate) fn validation_error(e: ValidationError) -> ApiError {
    match e {
        ValidationError::Cpu(e) => cpu_error(e),
        ValidationError::Numa(e) => crate::numa::numa_error(e),
        e @ ValidationError::NicQos(..) => {
            ApiError::bad_request(e.to_string()).with_code("invalid_qos")
        }
        e => ApiError::bad_request(e.to_string()),
    }
}

fn arch_name(arch: Arch) -> String {
    format!("{arch:?}").to_lowercase()
}

/// `host`·`max` 밖의 모델이나 플래그가 있으면 모든 노드가 `cpu-models`를 알아야 한다(옛 노드가 풀지 못한다).
pub(crate) fn require_cpu_feature(
    cluster: &opl_cluster::Cluster,
    model: &CpuModel,
    flags: &[CpuFlag],
) -> Result<(), ApiError> {
    if !model.is_host_or_max() || !flags.is_empty() {
        require_feature(cluster, CPU_MODELS)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------------------
// 검증과 별칭 고정
// ---------------------------------------------------------------------------------------------------------------

/// 아키텍처 `arch`의 위트니스 아닌 노드가 알린 카탈로그(노드 순서, 같은 해시는 한 번)
fn arch_catalogs(inv: &Inventory, arch: Arch) -> Vec<Arc<CpuCatalogSpec>> {
    let mut seen = BTreeSet::new();
    inv.nodes
        .iter()
        .filter(|n| !n.spec.witness && n.status.arch == Some(arch))
        .filter_map(|n| n.status.cpu.as_ref())
        .filter(|c| seen.insert(c.catalog.clone()))
        .filter_map(|c| inv.cpu_catalogs.get(&c.catalog).cloned())
        .collect()
}

/// 두 문자열의 편집 거리(대소문자 무시) — 비슷한 모델 이름 셋을 고른다.
fn distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_ascii_lowercase().chars().collect();
    let b: Vec<char> = b.to_ascii_lowercase().chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// 고른 모델과, 별칭에서 고정했으면 준 이름
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinnedModel {
    pub model: CpuModel,
    pub from: Option<String>,
}

/// 명명 모델 `name`을 이 아키텍처의 카탈로그들에서 찾아 버전 모델로 고정한다(§8). 대소문자는 카탈로그의 표기로
/// 맞춘다. 별칭의 뜻이 카탈로그마다 다르면 400 `cpu_model_ambiguous`, 어디에도 없으면 400 `unknown_cpu_model`(비슷한
/// 이름 셋), 다른 아키텍처의 모델이면 400 `cpu_model_arch`다.
pub(crate) fn pin_model(inv: &Inventory, arch: Arch, name: &str) -> Result<PinnedModel, ApiError> {
    let catalogs = arch_catalogs(inv, arch);
    let mut targets = BTreeSet::new();
    for c in &catalogs {
        if let Some(m) = c.models.iter().find(|m| m.name.eq_ignore_ascii_case(name)) {
            targets.insert(m.alias_of.clone().unwrap_or_else(|| m.name.clone()));
        }
    }
    let mut targets: Vec<String> = targets.into_iter().collect();
    match targets.len() {
        0 => {}
        1 => {
            let model = targets.remove(0);
            let from = (!model.eq_ignore_ascii_case(name)).then(|| name.to_string());
            let model = model.parse().map_err(cpu_error)?;
            return Ok(PinnedModel { model, from });
        }
        _ => {
            let mut e = ApiError::bad_request(format!(
                "CPU model {name} means {} on different nodes (their QEMU versions differ); give a versioned model",
                targets.join(" or ")
            ))
            .with_code("cpu_model_ambiguous");
            e.details = Some(serde_json::json!({ "candidates": targets }));
            return Err(e);
        }
    }
    let other = inv
        .cpu_catalogs
        .values()
        .find(|c| c.arch != arch && c.models.iter().any(|m| m.name.eq_ignore_ascii_case(name)));
    if let Some(c) = other {
        return Err(ApiError::bad_request(format!(
            "CPU model {name} is for {}, but the VM is {}",
            arch_name(c.arch),
            arch_name(arch)
        ))
        .with_code("cpu_model_arch"));
    }
    if catalogs.is_empty() {
        return Err(ApiError::bad_request(format!(
            "CPU model {name}: no {} node has reported its CPU models yet; use host or max",
            arch_name(arch)
        ))
        .with_code("unknown_cpu_model"));
    }
    let names: BTreeSet<&str> = catalogs
        .iter()
        .flat_map(|c| c.models.iter().map(|m| m.name.as_str()))
        .collect();
    let mut near: Vec<(usize, &str)> = names.into_iter().map(|n| (distance(n, name), n)).collect();
    near.sort();
    let near: Vec<&str> = near.into_iter().take(3).map(|(_, n)| n).collect();
    let mut e = ApiError::bad_request(format!(
        "unknown CPU model {name}; similar: {}",
        near.join(", ")
    ))
    .with_code("unknown_cpu_model");
    e.details = Some(serde_json::json!({ "similar": near }));
    Err(e)
}

/// 플래그 이름이 이 아키텍처 카탈로그들 가운데 하나에 있어야 한다(400 `unknown_cpu_flag`). 클러스터 숨김 목록의
/// 이름을 켜면(`+flag`) 400 `cpu_flag_masked`다. `known`(이미 저장된 플래그)은 다시 보지 않는다.
fn check_flags(
    inv: &Inventory,
    arch: Arch,
    flags: &[CpuFlag],
    known: &[CpuFlag],
) -> Result<(), ApiError> {
    let new: Vec<&CpuFlag> = flags.iter().filter(|f| !known.contains(f)).collect();
    if new.is_empty() {
        return Ok(());
    }
    let catalogs = arch_catalogs(inv, arch);
    for f in new {
        if !catalogs.iter().any(|c| c.flag_index(&f.name).is_some()) {
            return Err(ApiError::bad_request(format!(
                "unknown CPU flag {:?}: no {} node's QEMU knows it",
                f.name,
                arch_name(arch)
            ))
            .with_code("unknown_cpu_flag"));
        }
        if f.on && inv.cpu_mask.contains(&f.name) {
            return Err(ApiError::bad_request(format!(
                "CPU flag {:?} is hidden from every VM by the cluster setting cpu.mask",
                f.name
            ))
            .with_code("cpu_flag_masked"));
        }
    }
    Ok(())
}

/// 사용자가 아무 플래그나 켤(`+flag`) 수 있는지: 루트 `cluster.admin`(리뷰 2 #3)
pub(crate) fn may_turn_on_any_flag(state: &AppState, principal: &Principal) -> bool {
    state.authz.allows(
        principal,
        Privilege::ClusterAdmin,
        opl_types::authz_eval::AuthzTarget::Root,
    )
}

/// 루트 `cluster.admin`이 아닌 사용자는 [`cpu::USER_FLAGS`]만 켤 수 있다(리뷰 2 #3 — Proxmox의 VM 플래그 허용 목록).
/// 라이브 마이그레이션을 막는 `invtsc`나 중첩 가상화 `vmx`처럼 노드 관리에 닿는 플래그를 폴더 사용자가 켜지 못하게
/// 한다. 끄기(`-flag`)와 `known`(이미 저장된 플래그 — 관리자가 켠 것을 그대로 두는 PUT)은 보지 않는다. 아니면 403
/// `cpu_flag_restricted`다.
pub(crate) fn check_flag_privilege(
    state: &AppState,
    principal: &Principal,
    flags: &[CpuFlag],
    known: &[CpuFlag],
) -> Result<(), ApiError> {
    let restricted: Vec<String> = flags
        .iter()
        .filter(|f| f.on && !cpu::user_flag(&f.name) && !known.contains(f))
        .map(|f| f.to_string())
        .collect();
    if restricted.is_empty() || may_turn_on_any_flag(state, principal) {
        return Ok(());
    }
    let mut e = ApiError::new(
        StatusCode::FORBIDDEN,
        "cpu_flag_restricted",
        format!(
            "only a cluster administrator can turn on the CPU flags {}; others can turn on only {} (hiding any flag is allowed)",
            restricted.join(", "),
            cpu::USER_FLAGS.join(", ")
        ),
    );
    e.details = Some(serde_json::json!({ "flags": restricted, "allowed": cpu::USER_FLAGS }));
    Err(e)
}

/// VM의 CPU를 검증하고 명명 모델의 별칭을 버전 모델로 고정한다(§8). `previous`는 저장된 VM의 CPU다 — 모델이 그대로면
/// 다시 해석하지 않고 저장된 플래그는 다시 보지 않는다. 모델 문법·아키텍처·플래그 문법은 `VmSpec::validate`가 본다.
pub(crate) fn check_cpu(
    inv: &Inventory,
    arch: Arch,
    cpu: &CpuSpec,
    previous: Option<&CpuSpec>,
) -> Result<PinnedModel, ApiError> {
    let unchanged = previous.is_some_and(|p| p.model == cpu.model);
    let pinned = match &cpu.model {
        CpuModel::Named(name) if !unchanged => pin_model(inv, arch, name)?,
        CpuModel::Level(l) if arch != Arch::X86_64 => {
            return Err(cpu_error(CpuError::Arch {
                model: l.as_str().into(),
                arch,
            }));
        }
        m => PinnedModel {
            model: m.clone(),
            from: None,
        },
    };
    let known = previous.map(|p| p.flags.as_slice()).unwrap_or_default();
    check_flags(inv, arch, &cpu.flags, known)?;
    Ok(pinned)
}

/// 감사 설명의 CPU 부분: `cpu=Icelake-Server-v1 (from Icelake-Server) cpu.flags=-avx512vbmi`. 바뀐 것이 없으면 None.
pub(crate) fn audit_cpu(
    cpu: &CpuSpec,
    previous: Option<&CpuSpec>,
    from: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    let model_changed = previous.map_or(cpu.model != CpuModel::Host, |p| p.model != cpu.model);
    if model_changed || from.is_some() {
        let mut s = format!("cpu={}", cpu.model);
        if let Some(from) = from {
            s.push_str(&format!(" (from {from})"));
        }
        parts.push(s);
    }
    if previous.map_or(!cpu.flags.is_empty(), |p| p.flags != cpu.flags) {
        let flags: Vec<String> = cpu.flags.iter().map(|f| f.to_string()).collect();
        parts.push(format!("cpu.flags={}", flags.join(",")));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// 요청이 준 CPU(복제·가져오기·이미지 템플릿)의 감사 조각: `cpu=Icelake-Server-v1 (from Icelake-Server) cpu.flags=-x`
/// (리뷰 2 #7). `model`은 고정한 모델, `asked`는 요청에 적힌 이름이다. 준 것이 없으면 None.
pub(crate) fn audit_requested_cpu(
    model: Option<&CpuModel>,
    asked: Option<&CpuModel>,
    flags: &[CpuFlag],
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(m) = model {
        let mut s = format!("cpu={m}");
        if let Some(a) = asked.filter(|a| *a != m) {
            s.push_str(&format!(" (from {a})"));
        }
        parts.push(s);
    }
    if !flags.is_empty() {
        let flags: Vec<String> = flags.iter().map(|f| f.to_string()).collect();
        parts.push(format!("cpu.flags={}", flags.join(",")));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

// ---------------------------------------------------------------------------------------------------------------
// 노드마다 켤 수 있는지
// ---------------------------------------------------------------------------------------------------------------

/// 노드 하나가 CPU를 켤 수 있는지
#[derive(Debug, Clone, Serialize, ToSchema, PartialEq, Eq)]
pub struct CpuNodeFit {
    pub node: String,
    pub can_run: bool,
    /// 빠진 플래그(끄지 않았는데 가속기가 못 주는 것, 켰는데 지원하지 않는 것)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub missing: Vec<String>,
    /// 켤 수 없는 까닭
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reason: Option<String>,
    /// 막지 않는 경고(`host` VM을 옮기면 대상에 없는 플래그)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub warnings: Vec<String>,
}

/// 해석한 CPU의 빠진 플래그(노드 카탈로그가 있을 때)
fn missing_flags(ctx: &CpuContext, resolved: &ResolvedCpu) -> Vec<String> {
    match &ctx.catalog {
        NodeCatalog::Found(c) => cpu::can_run(resolved, c)
            .err()
            .map(|m| m.missing)
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 스펙 `cpu`를 노드에서 새로 켤 수 있는지(작업자의 켤 때 확인과 같은 규칙)
fn start_fit(inv: &Inventory, node: &Node, cpu: &CpuSpec) -> CpuNodeFit {
    let mut fit = CpuNodeFit {
        node: node.name.clone(),
        can_run: true,
        missing: Vec::new(),
        reason: None,
        warnings: Vec::new(),
    };
    let Some(ctx) = inv.cpu_context(node) else {
        if !cpu.model.is_host_or_max() || !cpu.flags.is_empty() {
            fit.can_run = false;
            fit.reason = Some(format!(
                "node {:?} has no CPU catalog; only the host and max CPU models can run there",
                node.name
            ));
        }
        return fit;
    };
    let resolved = match ctx.resolve(cpu) {
        Ok(r) => r,
        Err(e) => {
            fit.can_run = false;
            fit.reason = Some(format!("node {:?} cannot use the CPU: {e}", node.name));
            return fit;
        }
    };
    if let Err(e) = ctx.check(&resolved) {
        fit.can_run = false;
        fit.missing = missing_flags(&ctx, &resolved);
        fit.reason = Some(match e {
            CpuRefusal::Cannot(m) => m,
            CpuRefusal::Waiting(_) => format!(
                "node {:?} has no CPU catalog yet; only the host and max CPU models can run there",
                node.name
            ),
        });
    }
    fit
}

/// 모델 하나, 플래그 없는 vCPU 하나
fn model_spec(model: CpuModel) -> CpuSpec {
    CpuSpec {
        emulator: None,
        policy: Default::default(),
        thread_policy: None,
        sockets: 1,
        cores: 1,
        model,
        max_vcpus: None,
        flags: Vec::new(),
    }
}

/// 아키텍처 `arch`의 위트니스 아닌 노드(이름순)
fn arch_nodes(inv: &Inventory, arch: Arch) -> Vec<&Node> {
    let mut nodes: Vec<&Node> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness && n.status.arch == Some(arch))
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    nodes
}

// ---------------------------------------------------------------------------------------------------------------
// GET /vms/{key}/cpu
// ---------------------------------------------------------------------------------------------------------------

/// VM의 CPU: 스펙, 지금 켜면 쓸 것, 도는 것, 보류, 노드마다 켤 수 있는지
#[derive(Debug, Serialize, ToSchema)]
pub struct VmCpuInfo {
    pub spec: CpuSpec,
    /// 지금 VM의 노드에서 켜면 쓸 CPU(노드가 없거나 해석할 수 없으면 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub resolved: Option<ResolvedCpu>,
    /// 켤 때 기록한 CPU(도는 VM). 계획 63 전에 켠 VM은 모델만 편 것이다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub running: Option<ResolvedCpu>,
    /// 다음 전원 껐다 켜기 때 바뀔 것(`status.hardware.pending`의 `cpu`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pending: Option<PendingChange>,
    /// 이 아키텍처의 노드마다(이름순): 도는 VM은 옮길 수 있는지(기록한 CPU), 꺼진 VM은 켤 수 있는지(스펙). 루트
    /// `cluster.read`가 없으면 비어 있다(노드 이름을 보이지 않는다).
    pub nodes: Vec<CpuNodeFit>,
}

/// VM의 CPU와 노드마다 켤 수 있는지(마이그레이션 대화상자·`oplctl vm cpu`). 도는 VM은 켤 때 기록한 CPU로 옮길 수
/// 있는지를, 꺼진 VM은 스펙을 그 노드에서 해석해 켤 수 있는지를 본다. `host`·`max` VM을 옮기면 대상에 없는 플래그가
/// 경고다.
#[utoipa::path(get, path = "/vms/{key}/cpu", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = VmCpuInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_vm_cpu(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<VmCpuInfo>, ApiError> {
    let vm: Vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let inv = Inventory::load(&state.cluster)?;
    let mut info = vm_cpu_info(&inv, &vm);
    if !sees_node_names(&state, &principal) {
        info.nodes.clear();
    }
    Ok(Json(info))
}

fn vm_cpu_info(inv: &Inventory, vm: &Vm) -> VmCpuInfo {
    let arch = vm.spec.machine.arch();
    let here = vm
        .status
        .node
        .or(vm.spec.node)
        .and_then(|id| inv.nodes.iter().find(|n| n.id == id));
    let hw = vm.status.hardware.as_ref();
    let running = hw.map(|hw| match &hw.cpu_resolved {
        Some(r) => r.clone(),
        None => {
            let accel = here
                .and_then(|n| cpu::Accel::parse(&n.status.hypervisor))
                .unwrap_or(cpu::Accel::Kvm);
            cpu::resolve(&hw.cpu, arch, accel, None, &[], None).unwrap_or(ResolvedCpu {
                model: hw.cpu.model.to_string(),
                flags: BTreeMap::new(),
                enforce: false,
                source: cpu::CpuSource::Spec,
                dropped_mask: Vec::new(),
            })
        }
    });
    let resolved = here
        .and_then(|n| inv.cpu_context(n))
        .and_then(|ctx| ctx.resolve(&vm.spec.cpu).ok());
    let pending = hw.and_then(|hw| hw.pending.iter().find(|p| p.field == "cpu").cloned());
    let moving = hw.is_some() && matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating);
    let nodes = arch_nodes(inv, arch)
        .into_iter()
        .map(|node| {
            if !moving {
                return start_fit(inv, node, &vm.spec.cpu);
            }
            let mut fit = CpuNodeFit {
                node: node.name.clone(),
                can_run: true,
                missing: Vec::new(),
                reason: None,
                warnings: Vec::new(),
            };
            if let Err(reason) = cpu_receives(vm, inv, node) {
                fit.can_run = false;
                fit.reason = Some(reason);
                if let (Some(ctx), Some(r)) = (inv.cpu_context(node), running.as_ref()) {
                    fit.missing = missing_flags(&ctx, r);
                }
            }
            if let Some(from) = here
                && from.id != node.id
                && let Some(w) = host_cpu_warning(vm, inv, from, node)
            {
                fit.warnings.push(w);
            }
            fit
        })
        .collect();
    VmCpuInfo {
        spec: vm.spec.cpu.clone(),
        resolved,
        running,
        pending,
        nodes,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 노드
// ---------------------------------------------------------------------------------------------------------------

/// 노드 카탈로그의 모델 하나
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeCpuModel {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub alias_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub vendor: Option<String>,
    /// 플래그를 끄지 않고 이 노드에서 켤 수 있다.
    pub usable: bool,
    /// 이 가속기가 못 주는 플래그(끄면 켤 수 있다)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub missing: Vec<String>,
    /// 플래그 아닌 까닭(`type`, aarch64 KVM·HVF의 `accel`)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub missing_other: Vec<String>,
    pub migration_safe: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[schema(default)]
    pub deprecated: bool,
}

/// 노드 QEMU가 아는 플래그 하나
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeCpuFlag {
    pub name: String,
    /// 이 가속기가 줄 수 있다(`full max`).
    pub supported: bool,
}

/// 노드의 CPU: 탐침 결과와 카탈로그를 펼친 것
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeCpuInfo {
    pub node: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub arch: Option<Arch>,
    /// 탐침 결과(없으면 아직 읽지 않았거나 업그레이드 중 — `host`·`max`만)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub cpu: Option<NodeCpu>,
    /// 이 노드가 클러스터 CPU 기준을 켤 수 없는 까닭(`cluster` VM을 받지 않는다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub baseline_problem: Option<String>,
    /// 카탈로그의 모델(이름순, 카탈로그가 없으면 빈 것)
    pub models: Vec<NodeCpuModel>,
    /// 카탈로그의 플래그(이름순)
    pub flags: Vec<NodeCpuFlag>,
}

/// 노드의 CPU 탐침 결과(QEMU 버전·가속기·호스트 CPU·읽은 시각·오류)와 카탈로그(모델마다 켤 수 있는지·빠진 플래그,
/// 플래그마다 이 가속기가 주는지)
#[utoipa::path(get, path = "/nodes/{key}/cpu", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = NodeCpuInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_node_cpu(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<NodeCpuInfo>, ApiError> {
    let node: Node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let inv = Inventory::load(&state.cluster)?;
    Ok(Json(node_cpu_info(&inv, &node)))
}

fn node_cpu_info(inv: &Inventory, node: &Node) -> NodeCpuInfo {
    let catalog = node
        .status
        .cpu
        .as_ref()
        .and_then(|c| inv.cpu_catalogs.get(&c.catalog));
    let models = catalog
        .map(|c| {
            c.models
                .iter()
                .map(|m| {
                    let missing: Vec<String> = c.names(&m.missing).map(String::from).collect();
                    NodeCpuModel {
                        name: m.name.clone(),
                        alias_of: m.alias_of.clone(),
                        vendor: m.vendor.clone(),
                        usable: missing.is_empty() && m.missing_other.is_empty(),
                        missing,
                        missing_other: m.missing_other.clone(),
                        migration_safe: m.migration_safe,
                        deprecated: m.deprecated,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let flags = catalog
        .map(|c| {
            c.flags
                .iter()
                .enumerate()
                .map(|(i, f)| NodeCpuFlag {
                    name: f.clone(),
                    supported: c.supported.contains(i),
                })
                .collect()
        })
        .unwrap_or_default();
    let baseline_problem = node.status.arch.and_then(|arch| {
        inv.cpu_baselines.get(&arch)?;
        let fit = start_fit(inv, node, &model_spec(CpuModel::Cluster));
        fit.reason
    });
    NodeCpuInfo {
        node: node.name.clone(),
        arch: node.status.arch,
        cpu: node.status.cpu.clone(),
        baseline_problem,
        models,
        flags,
    }
}

/// 노드의 QEMU에 CPU 모델·플래그를 다시 묻는 Task(`node-cpu-probe`, 그 노드에서 돈다)를 돌려준다(202). 탐침은 보통
/// QEMU 바이너리가 바뀔 때 스스로 한다. 끝나지 않은 다시 읽기가 있으면 그것이다. 위트니스는 400, 오프라인이면 409
/// `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`.
#[utoipa::path(post, path = "/nodes/{key}/cpu/refresh", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_node_cpu(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    require_feature(&state.cluster, CPU_MODELS)?;
    if node.spec.witness {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness node and runs no VMs",
            node.name
        )));
    }
    crate::backups::check_online(&state, node.id)?;
    let (_, tasks) = state.cluster.list::<TaskSpec>()?;
    if let Some(task) = tasks.into_iter().find(|t| {
        t.spec.op == TaskOp::NodeCpuProbe && t.spec.node == node.id && !t.status.state.is_finished()
    }) {
        return Ok((StatusCode::ACCEPTED, Json(task)));
    }
    let task = create_task(&state.cluster, TaskOp::NodeCpuProbe, node.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

// ---------------------------------------------------------------------------------------------------------------
// GET /cpu/models·/cpu/flags·/cpu/baseline
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CpuModelsQuery {
    /// 아키텍처(`x86_64`·`aarch64`). 없으면 노드들의 아키텍처(섞였으면 400)다.
    pub arch: Option<Arch>,
    /// 이 노드(id나 이름)에서 켤 수 있는지만 본다.
    pub node: Option<String>,
    /// 켤 수 있는 노드가 있는 모델만
    #[serde(default)]
    pub usable: bool,
}

/// 모델의 종류
#[derive(Debug, Clone, Copy, Serialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CpuModelKind {
    /// `host`·`max`
    Basic,
    /// `cluster`(클러스터 기준)
    Cluster,
    /// `x86-64-v2` … `v4`
    Level,
    /// QEMU 모델
    Qemu,
}

/// 모델 하나
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuModelEntry {
    pub name: String,
    pub kind: CpuModelKind,
    /// 별칭이면 가리키는 버전 모델(카탈로그마다 다르면 첫 것). VM에는 버전 모델로 저장된다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub alias_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[schema(default)]
    pub deprecated: bool,
    /// `cluster`: 지금 기준(`Skylake-Server-v5 +pcid`). 기준이 없으면(off, 업그레이드 중) 없다 — 그때 `cluster`는
    /// `host`처럼 켠다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub current: Option<String>,
    /// 켤 수 있는 노드 수
    pub usable: u32,
    /// 노드마다(루트 `cluster.read`가 있을 때만)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub nodes: Vec<CpuNodeFit>,
}

/// 아키텍처 하나의 CPU 모델
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuModels {
    pub arch: Arch,
    /// 이 아키텍처의 노드 수(위트니스 제외, `node`를 주면 1)
    pub nodes: u32,
    pub models: Vec<CpuModelEntry>,
}

/// 아키텍처를 정한다: 준 것, 아니면 노드들의 아키텍처(하나일 때)
fn pick_arch(inv: &Inventory, arch: Option<Arch>) -> Result<Arch, ApiError> {
    if let Some(a) = arch {
        return Ok(a);
    }
    let archs: BTreeSet<Arch> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter_map(|n| n.status.arch)
        .collect();
    match archs.len() {
        0 => Ok(Arch::native()),
        1 => Ok(*archs.iter().next().expect("one")),
        _ => Err(ApiError::bad_request(
            "the cluster has nodes of several architectures; give arch",
        )),
    }
}

/// 노드 이름을 보일 수 있는지(루트 `cluster.read`). VM 응답·오류의 노드마다 내용도 이 규칙을 따른다(리뷰 1 M2).
pub(crate) fn sees_node_names(state: &AppState, principal: &Principal) -> bool {
    state
        .authorize(principal, Action::Read, NodeSpec::KIND)
        .is_ok()
}

/// 노드 이름을 보일 수 있는지(루트 `cluster.read`). 없으면 VM을 만들거나 고칠 수 있어야 모델 목록을 본다.
fn sees_nodes(state: &AppState, principal: &Principal) -> Result<bool, ApiError> {
    if sees_node_names(state, principal) {
        return Ok(true);
    }
    state
        .authz
        .require_anywhere(principal, Privilege::VmCreate)
        .or_else(|_| state.authz.require_anywhere(principal, Privilege::VmConfig))?;
    Ok(false)
}

/// 아키텍처의 CPU 모델(§13): `host`·`max`·`cluster`(지금 기준)·`x86-64-v*`(x86_64)와 노드 카탈로그의 QEMU 모델(이름순,
/// 별칭 포함 — 별칭은 저장할 때 버전 모델로 고정된다). 모델마다 켤 수 있는 노드 수와, 루트 `cluster.read`가 있으면
/// 노드마다 켤 수 있는지·빠진 플래그다(숨김 목록·기준을 지금처럼 적용). VM을 만들거나 고칠 수 있는 사용자가 읽는다.
#[utoipa::path(get, path = "/cpu/models", tag = "cpu",
    params(CpuModelsQuery),
    responses((status = 200, body = CpuModels), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_cpu_models(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<CpuModelsQuery>,
) -> Result<Json<CpuModels>, ApiError> {
    let show = sees_nodes(&state, &principal)?;
    let inv = Inventory::load(&state.cluster)?;
    let arch = pick_arch(&inv, q.arch)?;
    let mut nodes = arch_nodes(&inv, arch);
    if let Some(key) = &q.node {
        // 노드 이름을 볼 수 없으면 노드로 거를 수도 없다(404·200으로 노드가 있는지 드러나지 않게, 리뷰 1 M2).
        if !show {
            return Err(ApiError::forbidden());
        }
        let node = lookup::<NodeSpec>(&state.cluster, key)?;
        nodes.retain(|n| n.id == node.id);
    }
    let mut out = cpu_models(&inv, arch, &nodes);
    if q.usable {
        out.models.retain(|m| m.usable > 0);
    }
    if !show {
        out.models.iter_mut().for_each(|m| m.nodes.clear());
    }
    Ok(Json(out))
}

fn cpu_models(inv: &Inventory, arch: Arch, nodes: &[&Node]) -> CpuModels {
    let mut entries: Vec<(CpuModelEntry, CpuModel)> = Vec::new();
    let entry = |name: &str, kind| CpuModelEntry {
        name: name.to_string(),
        kind,
        alias_of: None,
        vendor: None,
        deprecated: false,
        current: None,
        usable: 0,
        nodes: Vec::new(),
    };
    for m in [CpuModel::Host, CpuModel::Max] {
        entries.push((entry(m.as_str(), CpuModelKind::Basic), m));
    }
    let mut cluster = entry("cluster", CpuModelKind::Cluster);
    // 기준이 없을 때 `host`를 적으면 기준이 있는 것과 구별되지 않는다(웹이 새 VM을 `cluster`로 두는 기준, 리뷰 1 웹 #1).
    cluster.current = inv.cpu_baselines.get(&arch).map(Baseline::describe);
    entries.push((cluster, CpuModel::Cluster));
    if arch == Arch::X86_64 {
        for l in X86Level::ALL.into_iter().rev() {
            entries.push((entry(l.as_str(), CpuModelKind::Level), CpuModel::Level(l)));
        }
    }
    let mut named: BTreeMap<String, CpuModelEntry> = BTreeMap::new();
    for c in arch_catalogs(inv, arch) {
        for m in &c.models {
            named
                .entry(m.name.clone())
                .or_insert_with(|| CpuModelEntry {
                    alias_of: m.alias_of.clone(),
                    vendor: m.vendor.clone(),
                    deprecated: m.deprecated,
                    ..entry(&m.name, CpuModelKind::Qemu)
                });
        }
    }
    for (name, e) in named {
        let model = match &e.alias_of {
            Some(target) => target.parse(),
            None => name.parse(),
        };
        if let Ok(model) = model {
            entries.push((e, model));
        }
    }
    let models = entries
        .into_iter()
        .map(|(mut e, model)| {
            let spec = model_spec(model);
            e.nodes = nodes.iter().map(|n| start_fit(inv, n, &spec)).collect();
            e.usable = u32::try_from(e.nodes.iter().filter(|f| f.can_run).count()).unwrap_or(0);
            e
        })
        .collect();
    CpuModels {
        arch,
        nodes: u32::try_from(nodes.len()).unwrap_or(0),
        models,
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CpuFlagsQuery {
    /// 아키텍처. 없으면 노드들의 아키텍처(섞였으면 400)다.
    pub arch: Option<Arch>,
}

/// 플래그 하나
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuFlagEntry {
    pub name: String,
    /// 이 플래그를 줄 수 있는 노드 수(`full max`)
    pub supported: u32,
    /// 이 이름을 아는 노드 수(끌 수 있다)
    pub known: u32,
    /// 클러스터 숨김 목록에 있다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[schema(default)]
    pub masked: bool,
    /// 루트 `cluster.admin`만 켤 수 있다(관리자가 아닌 사용자의 허용 목록 밖, 리뷰 2 #3). 끄기는 누구나 된다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[schema(default)]
    pub restricted: bool,
    /// 켜면 QEMU가 라이브 마이그레이션을 막는다(`invtsc`). 그런 VM은 비우기·균형 조정·HA 검사가 옮기지 않는다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[schema(default)]
    pub blocks_migration: bool,
}

/// 아키텍처 하나의 CPU 플래그
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuFlags {
    pub arch: Arch,
    /// 카탈로그를 알린 노드 수
    pub nodes: u32,
    /// 이 사용자가 `restricted` 플래그도 켤 수 있다(루트 `cluster.admin`).
    pub turn_on_restricted: bool,
    pub flags: Vec<CpuFlagEntry>,
}

/// 아키텍처의 CPU 플래그(노드 QEMU가 아는 이름, 이름순)와 그것을 줄 수 있는 노드 수. VM 플래그·숨김 목록 입력의 자동
/// 완성에 쓴다.
#[utoipa::path(get, path = "/cpu/flags", tag = "cpu",
    params(CpuFlagsQuery),
    responses((status = 200, body = CpuFlags), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_cpu_flags(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<CpuFlagsQuery>,
) -> Result<Json<CpuFlags>, ApiError> {
    sees_nodes(&state, &principal)?;
    let inv = Inventory::load(&state.cluster)?;
    let arch = pick_arch(&inv, q.arch)?;
    let admin = may_turn_on_any_flag(&state, &principal);
    Ok(Json(cpu_flags(&inv, arch, admin)))
}

fn cpu_flags(inv: &Inventory, arch: Arch, admin: bool) -> CpuFlags {
    let mut counts: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    let mut probed = 0;
    for node in arch_nodes(inv, arch) {
        let Some(c) = node
            .status
            .cpu
            .as_ref()
            .and_then(|c| inv.cpu_catalogs.get(&c.catalog))
        else {
            continue;
        };
        probed += 1;
        for (i, f) in c.flags.iter().enumerate() {
            let e = counts.entry(f.clone()).or_default();
            e.1 += 1;
            if c.supported.contains(i) {
                e.0 += 1;
            }
        }
    }
    CpuFlags {
        arch,
        nodes: probed,
        turn_on_restricted: admin,
        flags: counts
            .into_iter()
            .map(|(name, (supported, known))| CpuFlagEntry {
                masked: inv.cpu_mask.contains(&name),
                restricted: !cpu::user_flag(&name),
                blocks_migration: cpu::MIGRATION_BLOCKERS.contains(&name.as_str()),
                name,
                supported,
                known,
            })
            .collect(),
    }
}

/// 아키텍처 하나의 클러스터 기준
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuBaselineInfo {
    pub arch: Arch,
    /// 설정 `cpu.pinned`에 고정했다.
    pub pinned: bool,
    /// 리더가 기록한 기준(없으면 `cluster` VM은 `host`처럼 켠다)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub baseline: Option<Baseline>,
    /// `Skylake-Server-v5 +pcid` 모양
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub cpu: Option<String>,
    /// 셈에 든 노드 이름
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub nodes: Vec<String>,
    /// 기준을 낮춘(고정이면 켤 수 없는) 노드 이름
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub limited_by: Vec<String>,
    /// `cluster` 모델 VM 수
    pub cluster_vms: u32,
}

/// 클러스터 CPU 기준
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuBaselines {
    pub mode: BaselineMode,
    pub auto_flags: bool,
    /// 모든 VM에서 숨기는 플래그
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[schema(default)]
    pub mask: Vec<String>,
    /// 노드가 있거나 기준이 있는 아키텍처마다
    pub arches: Vec<CpuBaselineInfo>,
}

/// 클러스터 CPU 기준(§9): 설정의 모드·자동 플래그·숨김 목록과 아키텍처마다 리더가 기록한 기준·셈에 든 노드·낮춘 노드·
/// 경고·`cluster` VM 수
#[utoipa::path(get, path = "/cpu/baseline", tag = "cpu",
    responses((status = 200, body = CpuBaselines), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cpu_baseline(
    State(state): State<Arc<AppState>>,
) -> Result<Json<CpuBaselines>, ApiError> {
    let inv = Inventory::load(&state.cluster)?;
    let settings = crate::settings::current(&state.cluster)?
        .spec
        .cpu
        .unwrap_or_default();
    Ok(Json(cpu_baselines(&inv, &settings)))
}

fn cpu_baselines(inv: &Inventory, settings: &CpuSettings) -> CpuBaselines {
    let mut arches: BTreeSet<Arch> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter_map(|n| n.status.arch)
        .collect();
    arches.extend(inv.cpu_baselines.keys().copied());
    let name = |id: &uuid::Uuid| {
        inv.nodes
            .iter()
            .find(|n| n.id == *id)
            .map_or_else(|| id.to_string(), |n| n.name.clone())
    };
    CpuBaselines {
        mode: settings.baseline,
        auto_flags: settings.auto_flags,
        mask: settings.mask.clone(),
        arches: arches
            .into_iter()
            .map(|arch| {
                let b = inv.cpu_baselines.get(&arch);
                let cluster_vms = inv
                    .vms
                    .iter()
                    .filter(|v| {
                        v.spec.cpu.model == CpuModel::Cluster && v.spec.machine.arch() == arch
                    })
                    .count();
                CpuBaselineInfo {
                    arch,
                    pinned: settings.pinned_for(arch).is_some(),
                    cpu: b.map(Baseline::describe),
                    nodes: b.iter().flat_map(|b| &b.nodes).map(name).collect(),
                    limited_by: b.iter().flat_map(|b| &b.limited_by).map(name).collect(),
                    baseline: b.cloned(),
                    cluster_vms: u32::try_from(cluster_vms).unwrap_or(u32::MAX),
                }
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 설정 `cpu`
// ---------------------------------------------------------------------------------------------------------------

/// 설정 `cpu`에서 바꿀 값. 준 칸만 바꾼다.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CpuSettingsPatch {
    /// `off`(기본)이나 `auto`(리더가 모든 노드가 켤 수 있는 가장 좋은 모델을 셈함, x86_64만)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub baseline: Option<BaselineMode>,
    /// 아키텍처마다 고정한 기준 전체(`[]`는 지운다). 별칭은 버전 모델로 고정된다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub pinned: Option<Vec<cpu::PinnedBaseline>>,
    /// auto 기준에 모든 노드가 주는 플래그를 더한다(기본 참).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub auto_flags: Option<bool>,
    /// 모든 VM(`host` 포함)에서 숨길 플래그 전체(`[]`는 지운다). 다음 전원 껐다 켜기부터 적용된다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub mask: Option<Vec<String>>,
    /// 고정 기준을 켤 수 없는 노드가 있어도 저장한다(그 노드는 `cluster` VM을 받지 않는다).
    #[serde(default)]
    #[schema(default)]
    pub force: bool,
}

/// 패치를 `current`에 합치고 검증한다: 숨김 목록을 맞추고(정렬, 겹침 없이), 고정 모델의 별칭을 버전 모델로 고정하고,
/// 고정 모델·플래그·숨김 이름이 노드 카탈로그에 있는지, 고정 기준을 켤 수 없는 노드가 있으면 409
/// `cpu_baseline_unsupported`(`force`로 넘음)다. 감사 설명의 조각도 돌려준다.
pub(crate) fn apply_settings(
    inv: &Inventory,
    current: &CpuSettings,
    patch: &CpuSettingsPatch,
) -> Result<(CpuSettings, Vec<String>), ApiError> {
    let mut next = current.clone();
    let mut notes = Vec::new();
    if let Some(mode) = patch.baseline {
        next.baseline = mode;
        notes.push(format!(
            "cpu.baseline={}",
            match mode {
                BaselineMode::Off => "off",
                BaselineMode::Auto => "auto",
            }
        ));
    }
    if let Some(on) = patch.auto_flags {
        next.auto_flags = on;
        notes.push(format!("cpu.auto_flags={on}"));
    }
    if let Some(mask) = &patch.mask {
        next.mask = cpu::normalize_mask(mask).map_err(cpu_error)?;
        for name in &next.mask {
            if !inv
                .cpu_catalogs
                .values()
                .any(|c| c.flag_index(name).is_some())
            {
                return Err(ApiError::bad_request(format!(
                    "cpu.mask: unknown CPU flag {name:?}: no node's QEMU knows it"
                ))
                .with_code("unknown_cpu_flag"));
            }
        }
        notes.push(format!("cpu.mask={}", next.mask.join(",")));
    }
    if let Some(pinned) = &patch.pinned {
        let mut out = Vec::new();
        for p in pinned {
            let mut p = p.clone();
            if let CpuModel::Named(name) = &p.model {
                p.model = pin_model(inv, p.arch, name)?.model;
            }
            out.push(p);
        }
        next.pinned = out;
        let described: Vec<String> = next
            .pinned
            .iter()
            .map(|p| {
                let mut s = format!("{}:{}", arch_name(p.arch), p.model);
                for f in &p.flags {
                    s.push_str(&format!(",{f}"));
                }
                s
            })
            .collect();
        notes.push(format!("cpu.pinned={}", described.join(";")));
    }
    // 켤 수 없는 노드를 두고 고정했음을 남긴다(리뷰 2 #7).
    if patch.force && patch.pinned.is_some() {
        notes.push("cpu.force=true".into());
    }
    next.validate().map_err(cpu_error)?;
    for p in &next.pinned {
        check_flags(inv, p.arch, &p.flags, &[])?;
    }
    if patch.pinned.is_some() && !patch.force {
        let fresh = new_baselines(inv, &next);
        for p in &next.pinned {
            let Some(b) = fresh.get(&p.arch) else {
                continue;
            };
            if b.limited_by.is_empty() {
                continue;
            }
            let names: Vec<String> = b
                .limited_by
                .iter()
                .filter_map(|id| inv.nodes.iter().find(|n| n.id == *id))
                .map(|n| n.name.clone())
                .collect();
            let mut e = ApiError::conflict(
                "cpu_baseline_unsupported",
                format!(
                    "{}; give force to pin it anyway (those nodes will not run cluster VMs)",
                    b.warnings.join("; ")
                ),
            );
            e.details = Some(serde_json::json!({ "nodes": names, "warnings": b.warnings }));
            return Err(e);
        }
    }
    Ok((next, notes))
}

/// 설정 `settings`로 셈한 기준(리더가 다음 tick에 기록할 것)
fn new_baselines(inv: &Inventory, settings: &CpuSettings) -> BTreeMap<Arch, Baseline> {
    let catalogs: BTreeMap<String, CpuCatalogSpec> = inv
        .cpu_catalogs
        .iter()
        .map(|(k, v)| (k.clone(), v.as_ref().clone()))
        .collect();
    // 리더가 기록할 것과 같은 셈(빠진 카탈로그 동안의 붙잡기 포함, 리뷰 2 #2)
    crate::cpu_baseline::compute(
        &inv.nodes,
        &catalogs,
        settings,
        &inv.cpu_baselines,
        crate::auth::unix_now(),
    )
}

/// 설정 `cpu`를 `next`로 바꾼 뒤 보류 `cpu`(다음 전원 껐다 켜기에 CPU가 바뀜)를 가질 실행 중 VM 수(§11 — 화면의
/// "CPU 변경 대기" 목록과 같다). 리더는 다음 tick에야 기준을 다시 기록하고 노드 검사 루프가 그 뒤에 보류를 쓰므로 여기서
/// 셈한다: VM이 도는 노드에서 새 설정으로 스펙을 해석해 켤 때 기록과 견준다.
pub(crate) fn affected_running_vms(inv: &Inventory, next: &CpuSettings) -> u32 {
    let baselines = new_baselines(inv, next);
    let n = inv
        .vms
        .iter()
        .filter(|vm| matches!(vm.status.phase, VmPhase::Running | VmPhase::Migrating))
        .filter(|vm| {
            let Some(hw) = &vm.status.hardware else {
                return false;
            };
            let Some(node) = vm
                .status
                .node
                .and_then(|id| inv.nodes.iter().find(|n| n.id == id))
            else {
                return false;
            };
            let lookup = |hash: &str| inv.cpu_catalogs.get(hash).cloned();
            let wanted = CpuContext::from_parts(&inv.nodes, node, lookup, &baselines, &next.mask)
                .and_then(|c| c.wanted(&vm.spec.cpu));
            cpu::pending_change(&vm.spec.cpu, hw, wanted.as_ref()).is_some()
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_cpu_audit_names_the_pinned_model_and_flags() {
        let ice: CpuModel = "Icelake-Server-v1".parse().unwrap();
        let alias: CpuModel = "Icelake-Server".parse().unwrap();
        let flags: Vec<CpuFlag> = vec!["-avx512vbmi".parse().unwrap()];
        assert_eq!(
            audit_requested_cpu(Some(&ice), Some(&alias), &flags).as_deref(),
            Some("cpu=Icelake-Server-v1 (from Icelake-Server) cpu.flags=-avx512vbmi")
        );
        assert_eq!(
            audit_requested_cpu(Some(&ice), Some(&ice), &[]).as_deref(),
            Some("cpu=Icelake-Server-v1")
        );
        assert_eq!(audit_requested_cpu(None, None, &[]), None);
    }

    #[test]
    fn distance_counts_edits_ignoring_case() {
        assert_eq!(distance("Icelake-Server", "icelake-server"), 0);
        assert_eq!(distance("Icelake-Server-v7", "Icelake-Server-v1"), 1);
        assert_eq!(distance("abc", ""), 3);
        assert_eq!(distance("Skylake", "Skylark"), 2);
    }

    #[test]
    fn cpu_errors_have_the_design_codes() {
        let code = |e| cpu_error(e).code;
        assert_eq!(code(CpuError::ModelName("x".into())), "unknown_cpu_model");
        assert_eq!(
            code(CpuError::Arch {
                model: "x86-64-v3".into(),
                arch: Arch::Aarch64
            }),
            "cpu_model_arch"
        );
        assert_eq!(
            code(CpuError::DuplicateFlag("aes".into())),
            "invalid_cpu_flags"
        );
        assert_eq!(code(CpuError::TooManyFlags), "invalid_cpu_flags");
        assert_eq!(
            validation_error(ValidationError::Cpu(CpuError::FlagName("x".into()))).code,
            "invalid_cpu_flags"
        );
    }

    #[test]
    fn audit_text_names_the_pinned_model_and_changed_flags() {
        let spec = |model: &str, flags: &[&str]| CpuSpec {
            flags: flags.iter().map(|f| f.parse().unwrap()).collect(),
            ..model_spec(model.parse().unwrap())
        };
        let new = spec("Icelake-Server-v1", &["-avx512vbmi"]);
        assert_eq!(
            audit_cpu(&new, None, Some("Icelake-Server")).as_deref(),
            Some("cpu=Icelake-Server-v1 (from Icelake-Server) cpu.flags=-avx512vbmi")
        );
        let old = spec("Icelake-Server-v1", &[]);
        assert_eq!(
            audit_cpu(&new, Some(&old), None).as_deref(),
            Some("cpu.flags=-avx512vbmi")
        );
        assert_eq!(audit_cpu(&new, Some(&new), None), None);
        assert_eq!(audit_cpu(&spec("host", &[]), None, None), None);
    }
}
