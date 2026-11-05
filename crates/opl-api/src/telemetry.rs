//! 텔레메트리 보고 내용과 `GET /api/v1/telemetry/preview`. 수만 보내고 이름·주소·MAC·키·사용자·고객
//! 이름은 넣지 않는다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use opl_cluster::{Cluster, ClusterError};
use opl_types::backup_job::BackupJobSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::security_group::SecurityGroupSpec;
use opl_types::settings::SettingsSpec;
use opl_types::storage::StoragePoolSpec;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::{ImportSource, VmImportSpec};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::state::AppState;

/// 보고 형식 판
const SCHEMA: u32 = 1;

/// 텔레메트리 보고. 맵은 종류 → 수다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TelemetryReport {
    /// 보고 형식 판(지금 1)
    pub schema: u32,
    /// 설정 상태의 텔레메트리 id(아직 없으면 nil UUID)
    pub id: Uuid,
    /// 보낸 시각(유닉스 초)
    pub sent_at: u64,
    /// 버전별 노드 수(위트니스 포함, 버전을 적지 않은 노드는 `unknown`)
    pub versions: BTreeMap<String, u64>,
    pub nodes: NodeCounts,
    /// 아키텍처별 노드 수(위트니스 제외, 모르면 `unknown`)
    pub arch: BTreeMap<String, u64>,
    /// 하이퍼바이저별 노드 수(위트니스 제외)
    pub hypervisor: BTreeMap<String, u64>,
    /// CPU 합(위트니스 제외)
    pub cpus: u64,
    /// 메모리 합(위트니스 제외, GiB 내림)
    pub memory_gib: u64,
    pub vms: VmCounts,
    /// 백엔드 종류별 스토리지 풀 수
    pub pools: BTreeMap<String, u64>,
    /// 종류별 네트워크 수: `vxlan`, `vlan`(브리지와 VLAN), `bridge`
    pub networks: BTreeMap<String, u64>,
    pub security_groups: u64,
    pub backup_jobs: u64,
    /// 원본 종류별 VM 가져오기 수(`ova`, `esxi`)
    pub imports: BTreeMap<String, u64>,
    /// 구독 수준(`standard`, `premium`). 서명이 맞으면 만료돼도 수준이고, 없으면 `none`.
    pub subscription: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NodeCounts {
    pub total: u64,
    pub witness: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct VmCounts {
    pub total: u64,
    /// 지금 도는 VM(phase running)
    pub running: u64,
    /// HA를 켠 VM
    pub ha: u64,
}

fn count(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

fn len<T>(items: &[T]) -> u64 {
    items.len() as u64
}

/// 클러스터의 지금 상태로 보고를 만든다.
pub(crate) async fn build_report(
    cluster: &Cluster,
    id: Uuid,
    now: u64,
) -> Result<TelemetryReport, ClusterError> {
    let nodes = cluster.list::<NodeSpec>()?.1;
    let mut versions = BTreeMap::new();
    let (mut arch, mut hypervisor) = (BTreeMap::new(), BTreeMap::new());
    let (mut cpus, mut memory_mib, mut witness) = (0u64, 0u64, 0u64);
    for node in &nodes {
        count(
            &mut versions,
            node.status.version.as_deref().unwrap_or("unknown"),
        );
        if node.spec.witness {
            witness += 1;
            continue;
        }
        count(
            &mut arch,
            match node.status.arch {
                Some(Arch::X86_64) => "x86_64",
                Some(Arch::Aarch64) => "aarch64",
                None => "unknown",
            },
        );
        count(&mut hypervisor, &node.status.hypervisor);
        cpus += u64::from(node.status.cpus);
        memory_mib += node.status.memory_mib;
    }

    let vms = cluster.list::<VmSpec>()?.1;
    let mut pools = BTreeMap::new();
    for pool in cluster.list::<StoragePoolSpec>()?.1 {
        // 백엔드의 태그(`kind`)만 쓴다. 경로·서버 주소는 넣지 않는다.
        let kind = serde_json::to_value(&pool.spec.backend)?["kind"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        count(&mut pools, &kind);
    }
    let mut networks = BTreeMap::new();
    for network in cluster.list::<NetworkSpec>()?.1 {
        let kind = if network.spec.vxlan.is_some() {
            "vxlan"
        } else if network.spec.vlan.is_some() {
            "vlan"
        } else {
            "bridge"
        };
        count(&mut networks, kind);
    }
    let mut imports = BTreeMap::new();
    for import in cluster.list::<VmImportSpec>()?.1 {
        let kind = match import.spec.source {
            ImportSource::Ova(_) => "ova",
            ImportSource::Esxi(_) => "esxi",
        };
        count(&mut imports, kind);
    }
    let settings = crate::settings::current(cluster)?;
    let used = crate::subscription::used_nodes(cluster)?;
    let subscription = crate::subscription::state_of(&settings.spec, used)
        .level
        .map_or("none", |level| level.as_str())
        .to_string();

    Ok(TelemetryReport {
        schema: SCHEMA,
        id,
        sent_at: now,
        versions,
        nodes: NodeCounts {
            total: len(&nodes),
            witness,
        },
        arch,
        hypervisor,
        cpus,
        memory_gib: memory_mib / 1024,
        vms: VmCounts {
            total: len(&vms),
            running: vms
                .iter()
                .filter(|v| v.status.phase == VmPhase::Running)
                .count() as u64,
            ha: vms.iter().filter(|v| v.spec.ha).count() as u64,
        },
        pools,
        networks,
        security_groups: len(&cluster.list::<SecurityGroupSpec>()?.1),
        backup_jobs: len(&cluster.list::<BackupJobSpec>()?.1),
        imports,
        subscription,
    })
}

/// 지금 보낼 보고 그대로(꺼져 있어도). admin만 본다.
#[utoipa::path(get, path = "/telemetry/preview", tag = "settings",
    responses((status = 200, body = TelemetryReport), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn preview_telemetry(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<TelemetryReport>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let settings = crate::settings::current(&state.cluster)?;
    let id = settings
        .status
        .telemetry
        .and_then(|t| t.id)
        .unwrap_or_else(Uuid::nil);
    Ok(Json(build_report(&state.cluster, id, now()).await?))
}
