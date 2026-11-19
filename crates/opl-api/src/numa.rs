//! NUMA·전용 CPU·hugepages API(계획 64 §12·§13): VM 검증 오류(400 `invalid_numa`)와 권한 `vm.dedicated`, 노드
//! `host_resources`의 검증(400 `invalid_host_resources`)과 쓰기 전 확인(409 `host_cpus_in_use`·`hugepages_in_use`),
//! `GET /vms/{key}/numa`, `GET /nodes/{key}/topology`, `GET /topology/usage`.
//!
//! 노드 자원의 셈은 스케줄러의 장부([`Inventory::host_usage`] — 도는 VM의 기록된 배치는 정확히, 아직 켜지 않은 VM과
//! 오는 마이그레이션은 맞춰 본 배치)를 그대로 쓴다. 노드의 지금 값(빈 메모리·빈 hugepages·orbivisord 친화)은 그 노드에
//! 노드 사이 RPC `ReadTopology`로 묻는다(이 노드면 직접).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::types::NodeId;
use opl_types::authz::Privilege;
use opl_types::node::{NUMA, Node, NodeSpec};
use opl_types::numa::{
    self, CpuPolicy, CpuSet, EmulatorPolicy, HostPlacement, HostResources, HostSets, NodeTopology,
    NumaError, NumaSpec, PageKind, PageSize, ResolvedNuma, ThreadPolicy, TopologyLive,
};
use opl_types::resource::Kind;
use opl_types::vm::{PendingChange, Vm, VmSpec};
use serde::Serialize;
use utoipa::ToSchema;

use crate::auth::Principal;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json};
use crate::placement::Assign;
use crate::resources::lookup;
use crate::scheduler::{Inventory, numa_plan};
use crate::state::AppState;

/// 다른 노드에 지금 값을 묻는 시간
const LIVE_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------------------------------------------
// 검증·권한
// ---------------------------------------------------------------------------------------------------------------

/// VM 스펙의 NUMA 칸 오류(400 `invalid_numa`)
pub(crate) fn numa_error(e: NumaError) -> ApiError {
    ApiError::bad_request(e.to_string()).with_code("invalid_numa")
}

/// 계획 64 칸(CPU 정책·스레드·에뮬레이터·vNUMA·hugepages)이 있으면 모든 노드가 `numa`를 알아야 한다(옛 노드가 풀지
/// 못한다, 검증보다 먼저 — 409 `cluster_upgrading`).
pub(crate) fn require_numa_feature(
    cluster: &opl_cluster::Cluster,
    spec: &VmSpec,
) -> Result<(), ApiError> {
    if numa::in_use(spec) {
        require_feature(cluster, NUMA)?;
    }
    Ok(())
}

/// 권한 `vm.dedicated`(§12): 전용 CPU나 hugepages를 쓰는 스펙을 만들거나(`current` 없음 — 폴더 `spec.folder`에서) 그
/// 칸(정책·스레드·에뮬레이터·hugepages)을 바꿀 때(`current`의 VM에서) 요구한다. 없으면 403이다. vNUMA·메모리 묶기만
/// 쓰는 공유 VM과 그 칸을 그대로 두는 바꾸기(관리자가 켠 전용 VM의 vCPU 수 바꾸기 등)는 보지 않는다.
pub(crate) fn check_privilege(
    state: &AppState,
    principal: &Principal,
    spec: &VmSpec,
    current: Option<&Vm>,
) -> Result<(), ApiError> {
    match current {
        Some(vm) if numa::host_resources_changed(&vm.spec, spec) => state
            .authz
            .require(principal, Privilege::VmDedicated, vm)
            .map_err(dedicated_forbidden),
        Some(_) => Ok(()),
        None => require_privilege(
            state,
            principal,
            numa::uses_host_resources(spec),
            spec.folder,
        ),
    }
}

/// 새 VM(폴더 `folder`, 없으면 루트)이 노드 자원을 쓰면(`needed`) 그 폴더의 `vm.dedicated`(복제·배포·가져오기·템플릿)
pub(crate) fn require_privilege(
    state: &AppState,
    principal: &Principal,
    needed: bool,
    folder: Option<uuid::Uuid>,
) -> Result<(), ApiError> {
    if !needed {
        return Ok(());
    }
    state
        .authz
        .require_in_folder(principal, Privilege::VmDedicated, Kind::Vm, folder)
        .map_err(dedicated_forbidden)
}

/// 403의 문구를 권한 이름으로
fn dedicated_forbidden(e: ApiError) -> ApiError {
    if e.status == StatusCode::FORBIDDEN {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "dedicated CPUs and hugepages need the vm.dedicated privilege",
        )
    } else {
        e
    }
}

// ---------------------------------------------------------------------------------------------------------------
// 노드 설정 host_resources
// ---------------------------------------------------------------------------------------------------------------

/// `PUT /nodes/{key}`의 `host_resources`를 정한 결과
pub(crate) struct HostResourcesChange {
    /// 쓸 값
    pub value: Option<HostResources>,
    /// 감사 설명(바뀔 때만)
    pub audit: Option<String>,
    /// `force`로 넘긴 확인(응답 경고)
    pub warnings: Vec<String>,
}

/// 노드 `node`의 새 `host_resources`(§7·§13). 칸이 없으면(옛 클라이언트의 PUT) 저장된 값을 그대로 두고, `{}`(모두 빈
/// 것)면 지운다. 바뀌면: 모든 노드가 `numa`를 알아야 하고(409 `cluster_upgrading`), 위트니스·토폴로지가 없는 노드는 400
/// `invalid_host_resources`, 노드 토폴로지에 맞아야 하며(온라인 CPU, 겹치지 않음, 공유가 남음, 있는 호스트 노드 — 400
/// `invalid_host_resources`), 도는 VM이 고정된 pCPU가 새 전용 CPU 밖이면 409 `host_cpus_in_use`, Orbivisor VM이 쓰는
/// hugepages보다 적게 예약하면 409 `hugepages_in_use`다. `force`면 두 409를 넘기고 경고로 돌려준다.
pub(crate) fn host_resources_change(
    state: &AppState,
    node: &Node,
    asked: Option<HostResources>,
    force: bool,
) -> Result<HostResourcesChange, ApiError> {
    let stored = node.spec.host_resources.clone();
    let value = match asked {
        None => stored.clone(),
        Some(r) if r.is_empty() => None,
        Some(r) => Some(r),
    };
    if value == stored {
        return Ok(HostResourcesChange {
            value,
            audit: None,
            warnings: Vec::new(),
        });
    }
    let audit = Some(format!(
        "node set {} host_resources {}",
        node.name,
        value
            .as_ref()
            .map_or_else(|| "none".into(), HostResources::describe)
    ));
    let Some(r) = &value else {
        // 지우기: 공유 = 모든 CPU로 돌아간다. 도는 전용 VM은 다음 검사 회차에 온라인 CPU 전체로 풀린다.
        return Ok(HostResourcesChange {
            value,
            audit,
            warnings: Vec::new(),
        });
    };
    require_feature(&state.cluster, NUMA)?;
    let invalid = |m: String| ApiError::bad_request(m).with_code("invalid_host_resources");
    if node.spec.witness {
        return Err(invalid(format!(
            "node {:?} is a witness node and runs no VMs",
            node.name
        )));
    }
    let Some(topology) = node.status.topology.as_ref() else {
        return Err(invalid(format!(
            "node {:?} has not reported its NUMA topology (only Linux nodes have one)",
            node.name
        )));
    };
    r.validate(topology)
        .map_err(|e| invalid(format!("node {:?}: {e}", node.name)))?;
    let inv = Inventory::load(&state.cluster)?;
    let mut warnings = Vec::new();
    let pinned = pinned_outside(&inv, node, &r.dedicated_cpus);
    if !pinned.is_empty() {
        let names: Vec<&str> = pinned.iter().map(|(n, _)| n.as_str()).collect();
        let message = format!(
            "running VMs are pinned to CPUs outside the new dedicated CPUs of node {:?}: {}",
            node.name,
            pinned
                .iter()
                .map(|(n, cpus)| format!("{n} ({cpus})"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if !force {
            let mut e = ApiError::conflict("host_cpus_in_use", message);
            e.details = Some(serde_json::json!({ "vms": names }));
            return Err(e);
        }
        warnings.push(message);
    }
    let short = hugepages_short(&inv, node, topology, r);
    if !short.is_empty() {
        let message = format!(
            "Orbivisor VMs use more hugepages than node {:?} would reserve: {}",
            node.name,
            short.join(", ")
        );
        if !force {
            let mut e = ApiError::conflict("hugepages_in_use", message);
            e.details = Some(serde_json::json!({ "short": short }));
            return Err(e);
        }
        warnings.push(message);
    }
    Ok(HostResourcesChange {
        value,
        audit,
        warnings,
    })
}

/// 노드 `node`에서 도는 VM 가운데 기록된 고정 pCPU(vCPU·비워 둔 형제·`isolate` 에뮬레이터)가 `dedicated` 밖인 것(이름,
/// 밖의 CPU)
fn pinned_outside(inv: &Inventory, node: &Node, dedicated: &CpuSet) -> Vec<(String, CpuSet)> {
    let mut out = Vec::new();
    for vm in &inv.vms {
        let Some(p) = vm
            .status
            .hardware
            .as_ref()
            .and_then(|hw| hw.host_placement.as_ref())
            .filter(|p| p.node == node.id)
        else {
            continue;
        };
        let used: CpuSet = p.vcpus.iter().copied().collect();
        let used = used.union(&p.siblings).union(&p.emulator);
        let outside = used.difference(dedicated);
        if !outside.is_empty() {
            out.push((vm.name.clone(), outside));
        }
    }
    out.sort();
    out
}

/// 새 hugepages 예약이 Orbivisor VM이 쓰는 페이지보다 적은 것(`2M@0: 512 reserved, 600 used`). 호스트 노드를 정하지
/// 않은 예약은 모든 호스트 노드의 합과 견준다. 설정에서 뺀 크기는 예약을 줄이지 않으므로(지금 수를 그대로 둔다) 보지
/// 않는다.
fn hugepages_short(
    inv: &Inventory,
    node: &Node,
    topology: &NodeTopology,
    r: &HostResources,
) -> Vec<String> {
    let sets = HostSets::new(Some(r), topology);
    let (usage, _) = inv.host_usage(node, topology, &sets, uuid::Uuid::nil(), &Assign::default());
    r.hugepages
        .iter()
        .filter_map(|h| {
            let used: u64 = usage
                .pages
                .iter()
                .filter(|((n, k), _)| *k == h.size && h.host_node.is_none_or(|w| w == *n))
                .map(|(_, v)| *v)
                .sum();
            (u64::from(h.count) < used).then(|| {
                let at = h.host_node.map(|n| format!("@{n}")).unwrap_or_default();
                format!("{}{at}: {} reserved, {used} used", h.size, h.count)
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// GET /vms/{key}/numa
// ---------------------------------------------------------------------------------------------------------------

/// VM 스펙의 CPU 배치 칸
#[derive(Debug, Serialize, ToSchema)]
pub struct NumaSettings {
    pub policy: CpuPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub thread_policy: Option<ThreadPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub emulator: Option<EmulatorPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub numa: Option<NumaSpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub hugepages: Option<PageSize>,
}

/// 노드에서 쓸 배치(묶는 VM만)
#[derive(Debug, Serialize, ToSchema)]
pub struct NumaPlan {
    /// 게스트 노드마다 호스트 NUMA 노드
    pub host_nodes: Vec<u16>,
    /// 잡을 전용 pCPU(vCPU·비워 둘 형제·`isolate` 에뮬레이터, 공유 VM은 빔)
    pub pcpus: CpuSet,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub page: Option<PageKind>,
}

/// 노드 하나가 VM을 받거나 켤 수 있는지
#[derive(Debug, Serialize, ToSchema)]
pub struct NumaNodeFit {
    pub node: String,
    pub fits: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub plan: Option<NumaPlan>,
}

/// VM의 NUMA: 스펙, 지금 모양, 기록, 보류, 노드마다 맞는지
#[derive(Debug, Serialize, ToSchema)]
pub struct VmNumaInfo {
    pub spec: NumaSettings,
    /// 스펙의 짧은 이름(`2 nodes, dedicated, hugepages 1G`, 쓰지 않으면 `none`)
    pub wanted: String,
    /// 켤 때 기록한 게스트 모양(도는 VM, 계획 64 칸을 쓰지 않으면 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub running: Option<ResolvedNuma>,
    /// 켤 때 잡은 호스트 노드·pCPU. 루트 `cluster.read`가 없으면 없다(노드·pCPU 번호를 보이지 않는다).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub placement: Option<HostPlacement>,
    /// 다음 전원 껐다 켜기 때 바뀔 것(`status.hardware.pending`의 `numa`)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub pending: Option<PendingChange>,
    /// 이 아키텍처의 노드마다(이름순): 도는 VM(하드웨어 기록이 있음)은 기록 모양으로 옮길 수 있는지, 꺼진 VM은 스펙으로
    /// 켤 수 있는지. 루트 `cluster.read`가 없으면 비어 있다.
    pub nodes: Vec<NumaNodeFit>,
}

/// VM의 CPU 배치와 NUMA(마이그레이션 대화상자·`oplctl vm numa`): 스펙, 기록한 모양과 배치, 보류, 노드마다 맞는지와 그
/// 노드에서 쓸 배치(호스트 노드·pCPU·페이지)
#[utoipa::path(get, path = "/vms/{key}/numa", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = VmNumaInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_vm_numa(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<VmNumaInfo>, ApiError> {
    let vm: Vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let inv = Inventory::load(&state.cluster)?;
    let mut info = vm_numa_info(&inv, &vm);
    if !crate::cpu::sees_node_names(&state, &principal) {
        info.placement = None;
        info.nodes.clear();
    }
    Ok(Json(info))
}

fn vm_numa_info(inv: &Inventory, vm: &Vm) -> VmNumaInfo {
    let spec = &vm.spec;
    let hw = vm.status.hardware.as_ref();
    let arch = spec.machine.arch();
    let mut nodes: Vec<&Node> = inv
        .nodes
        .iter()
        .filter(|n| !n.spec.witness && n.status.arch == Some(arch))
        .collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let nodes = nodes
        .into_iter()
        .map(|node| match numa_plan(vm, inv, node) {
            Ok(fit) => NumaNodeFit {
                node: node.name.clone(),
                fits: true,
                reason: None,
                plan: fit.and_then(|f| {
                    let p = f.placement?;
                    let vcpus: CpuSet = p.vcpus.iter().copied().collect();
                    Some(NumaPlan {
                        pcpus: vcpus.union(&p.siblings).union(&p.emulator),
                        host_nodes: p.host_nodes,
                        page: f.numa.page,
                    })
                }),
            },
            Err(reason) => NumaNodeFit {
                node: node.name.clone(),
                fits: false,
                reason: Some(reason),
                plan: None,
            },
        })
        .collect();
    VmNumaInfo {
        spec: NumaSettings {
            policy: spec.cpu.policy,
            thread_policy: spec.cpu.thread_policy,
            emulator: spec.cpu.emulator,
            numa: spec.numa.clone(),
            hugepages: spec.hugepages,
        },
        wanted: numa::describe_spec(spec),
        running: hw.and_then(|hw| hw.numa.clone()),
        placement: hw.and_then(|hw| hw.host_placement.clone()),
        pending: hw.and_then(|hw| hw.pending.iter().find(|p| p.field == "numa").cloned()),
        nodes,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// GET /nodes/{key}/topology·/topology/usage
// ---------------------------------------------------------------------------------------------------------------

/// 노드의 세 CPU 집합(§7)
#[derive(Debug, Serialize, ToSchema)]
pub struct CpuSets {
    pub reserved: CpuSet,
    pub dedicated: CpuSet,
    pub shared: CpuSet,
}

/// 크기 하나의 hugepages(페이지 수)
#[derive(Debug, Serialize, ToSchema)]
pub struct PagesUse {
    pub size: PageKind,
    /// 풀의 페이지 수(노드가 알린 예약)
    pub total: u64,
    /// Orbivisor VM이 잡은 페이지 수
    pub used: u64,
}

/// 호스트 NUMA 노드 하나의 사용
#[derive(Debug, Serialize, ToSchema)]
pub struct HostNodeUse {
    pub id: u16,
    pub cpus: CpuSet,
    pub memory_mib: u64,
    /// 이 호스트 노드의 전용 CPU
    pub dedicated: CpuSet,
    /// 그 가운데 잡힌 것
    pub dedicated_used: CpuSet,
    pub hugepages: Vec<PagesUse>,
}

/// 이 노드에서 도는 VM 하나가 잡은 것(기록)
#[derive(Debug, Serialize, ToSchema)]
pub struct VmPlacementUse {
    pub vm: String,
    /// 게스트 노드마다 호스트 NUMA 노드(묶지 않으면 빔)
    pub host_nodes: Vec<u16>,
    /// 자리마다 pCPU(공유 VM은 빔)
    pub vcpus: Vec<u16>,
    /// `isolate`가 비워 둔 형제
    pub siblings: CpuSet,
    /// 에뮬레이터 스레드의 전용 pCPU(`isolate`, 아니면 빔)
    pub emulator: CpuSet,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub page: Option<PageKind>,
    /// 잡은 페이지 수(hugepages VM)
    pub pages: u64,
}

/// 노드의 토폴로지와 사용
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeTopologyInfo {
    pub node: String,
    /// 탐침 결과(Linux 노드, 모든 노드가 `numa`를 알린 뒤). 없으면 아래 칸이 빈다.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub topology: Option<NodeTopology>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub host_resources: Option<HostResources>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub sets: Option<CpuSets>,
    /// 호스트 NUMA 노드마다 사용(id 순)
    pub host_nodes: Vec<HostNodeUse>,
    /// 이 노드에서 도는 묶는 VM(이름순)
    pub vms: Vec<VmPlacementUse>,
    /// 노드에 직접 물은 지금 값(답이 없으면 없음)
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub live: Option<TopologyLive>,
    /// 호스트 준비가 덜 된 것(`topology.warnings`)
    pub warnings: Vec<String>,
}

/// 노드의 NUMA 토폴로지(소켓·NUMA 노드·L3·형제·거리·hugepages 풀·`isolcpus`)와 세 CPU 집합(예약·전용·공유), 호스트
/// 노드마다 사용(전용 CPU·hugepages), 이 노드에서 도는 VM이 잡은 pCPU·페이지, 노드에 직접 물은 지금 값(빈 메모리·빈
/// hugepages·orbivisord 친화, 노드 사이 RPC `ReadTopology` — 답이 없으면 `live`가 없다), 경고
#[utoipa::path(get, path = "/nodes/{key}/topology", tag = "nodes",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = NodeTopologyInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_node_topology(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
) -> Result<Json<NodeTopologyInfo>, ApiError> {
    let node: Node = lookup::<NodeSpec>(&state.cluster, &key)?;
    let inv = Inventory::load(&state.cluster)?;
    let mut info = node_topology_info(&inv, &node);
    if info.topology.is_some() {
        info.live = Box::pin(read_live(&state, &node)).await;
    }
    Ok(Json(info))
}

fn node_topology_info(inv: &Inventory, node: &Node) -> NodeTopologyInfo {
    let mut info = NodeTopologyInfo {
        node: node.name.clone(),
        topology: node.status.topology.clone(),
        host_resources: node.spec.host_resources.clone(),
        sets: None,
        host_nodes: Vec::new(),
        vms: Vec::new(),
        live: None,
        warnings: Vec::new(),
    };
    let Some(topology) = node.status.topology.as_ref() else {
        return info;
    };
    info.warnings = topology.warnings.clone();
    let sets = HostSets::new(node.spec.host_resources.as_ref(), topology);
    let (usage, _) = inv.host_usage(node, topology, &sets, uuid::Uuid::nil(), &Assign::default());
    info.host_nodes = topology
        .nodes
        .iter()
        .map(|h| {
            let dedicated = sets.dedicated.intersection(&h.cpus);
            HostNodeUse {
                id: h.id,
                cpus: h.cpus.clone(),
                memory_mib: h.memory_mib,
                dedicated_used: usage.dedicated.intersection(&dedicated),
                dedicated,
                hugepages: [PageKind::Size2M, PageKind::Size1G]
                    .into_iter()
                    .filter(|k| h.pages(*k) > 0 || usage.pages.contains_key(&(h.id, *k)))
                    .map(|k| PagesUse {
                        size: k,
                        total: h.pages(k),
                        used: usage.pages.get(&(h.id, k)).copied().unwrap_or(0),
                    })
                    .collect(),
            }
        })
        .collect();
    let mut vms: Vec<VmPlacementUse> = inv
        .vms
        .iter()
        .filter_map(|vm| {
            let hw = vm.status.hardware.as_ref()?;
            let p = hw.host_placement.as_ref().filter(|p| p.node == node.id)?;
            let n = hw.numa.as_ref()?;
            let pages = numa::usage([(n, p)], node.id).pages.values().sum();
            Some(VmPlacementUse {
                vm: vm.name.clone(),
                host_nodes: p.host_nodes.clone(),
                vcpus: p.vcpus.clone(),
                siblings: p.siblings.clone(),
                emulator: p.emulator.clone(),
                page: n.page,
                pages,
            })
        })
        .collect();
    vms.sort_by(|a, b| a.vm.cmp(&b.vm));
    info.vms = vms;
    info.sets = Some(CpuSets {
        reserved: sets.reserved,
        dedicated: sets.dedicated,
        shared: sets.shared,
    });
    info
}

/// 노드의 지금 값: 이 노드면 직접, 아니면 노드 사이 RPC로. 오프라인·옛 노드·실패·시간 넘김은 None이다.
async fn read_live(state: &AppState, node: &Node) -> Option<TopologyLive> {
    if node.id == state.node {
        let source = state.topology.clone()?;
        return match tokio::time::timeout(LIVE_TIMEOUT, source.live()).await {
            Ok(Ok(live)) => Some(live),
            Ok(Err(e)) => {
                tracing::warn!(node = %node.name, error = %e, "cannot read the live topology");
                None
            }
            Err(_) => None,
        };
    }
    if !node.status.online {
        return None;
    }
    let peers = state.peers.as_ref()?;
    let members = state.cluster.members();
    let addr = members.addr(NodeId(node.id))?;
    let read = peers.read_topology(NodeId(node.id), addr, LIVE_TIMEOUT);
    match tokio::time::timeout(LIVE_TIMEOUT, read).await {
        Ok(Ok(live)) => live,
        Ok(Err(status)) => {
            tracing::warn!(node = %node.name, error = %status.message(), "cannot read the live topology");
            None
        }
        Err(_) => None,
    }
}

/// 크기 하나의 hugepages 합(MiB)
#[derive(Debug, Serialize, ToSchema)]
pub struct PagesMib {
    pub size: PageKind,
    pub total_mib: u64,
    pub used_mib: u64,
}

/// 노드 목록 열용 요약
#[derive(Debug, Serialize, ToSchema)]
pub struct NodeUsageSummary {
    pub node: String,
    /// vCPU를 고정할 수 있다(토폴로지가 있는 Linux 노드)
    pub pinning: bool,
    pub dedicated_total: u64,
    pub dedicated_used: u64,
    /// 풀이 있거나 쓰는 크기만
    pub hugepages: Vec<PagesMib>,
}

/// `GET /topology/usage`
#[derive(Debug, Serialize, ToSchema)]
pub struct TopologyUsage {
    /// 위트니스가 아닌 노드(이름순)
    pub nodes: Vec<NodeUsageSummary>,
}

/// 노드마다 전용 CPU(전체·사용)와 크기마다 hugepages(전체·사용, MiB) — 노드 목록의 열과 VM 폼의 "들어갈 노드". 사용은
/// 스케줄러의 셈(도는 VM의 기록 + 아직 켜지 않은 VM·오는 마이그레이션을 맞춰 본 것)이다.
#[utoipa::path(get, path = "/topology/usage", tag = "nodes",
    responses((status = 200, body = TopologyUsage), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn topology_usage(
    State(state): State<Arc<AppState>>,
) -> Result<Json<TopologyUsage>, ApiError> {
    let inv = Inventory::load(&state.cluster)?;
    Ok(Json(usage_summary(&inv)))
}

fn usage_summary(inv: &Inventory) -> TopologyUsage {
    let mut nodes: Vec<&Node> = inv.nodes.iter().filter(|n| !n.spec.witness).collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let nodes = nodes
        .into_iter()
        .map(|node| {
            let mut out = NodeUsageSummary {
                node: node.name.clone(),
                pinning: false,
                dedicated_total: 0,
                dedicated_used: 0,
                hugepages: Vec::new(),
            };
            let Some(topology) = node.status.topology.as_ref() else {
                return out;
            };
            let sets = HostSets::new(node.spec.host_resources.as_ref(), topology);
            let (usage, _) =
                inv.host_usage(node, topology, &sets, uuid::Uuid::nil(), &Assign::default());
            out.pinning = topology.pinning;
            out.dedicated_total = sets.dedicated.len() as u64;
            out.dedicated_used = usage.dedicated.intersection(&sets.dedicated).len() as u64;
            let mut used: BTreeMap<PageKind, u64> = BTreeMap::new();
            for ((_, k), n) in &usage.pages {
                *used.entry(*k).or_default() += n;
            }
            out.hugepages = [PageKind::Size2M, PageKind::Size1G]
                .into_iter()
                .filter(|k| topology.pages(*k) > 0 || used.contains_key(k))
                .map(|k| {
                    let mib = u64::from(k.size_mib());
                    PagesMib {
                        size: k,
                        total_mib: topology.pages(k) * mib,
                        used_mib: used.get(&k).copied().unwrap_or(0) * mib,
                    }
                })
                .collect();
            out
        })
        .collect();
    TopologyUsage { nodes }
}
