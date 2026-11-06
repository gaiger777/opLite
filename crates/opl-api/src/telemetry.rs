//! 텔레메트리 보고 내용과 `GET /api/v1/telemetry/preview`, 그리고 리더가 보고를 보내는
//! [`TelemetryReporter`]. 수만 보내고 이름·주소·MAC·키·사용자·고객 이름은 넣지 않는다.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use opl_cluster::{Cluster, ClusterError};
use opl_types::backup_job::BackupJobSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::security_group::SecurityGroupSpec;
use opl_types::settings::SettingsSpec;
use opl_types::storage::StoragePoolSpec;
use opl_types::version::TELEMETRY_URL;
use opl_types::vm::{Arch, VmPhase, VmSpec};
use opl_types::vm_import::{ImportSource, VmImportSpec};
use serde::Serialize;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
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

/// 텔레메트리 리포터 설정
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// 보낼 때가 됐는지 확인하는 간격
    pub tick: Duration,
    /// 보내는 간격(마지막으로 보낸 뒤)
    pub interval: Duration,
    /// 실패한 뒤 다시 보내기까지 기다리는 시간
    pub retry: Duration,
    /// 요청 하나의 시간 제한
    pub timeout: Duration,
    /// 시스템 루트에 더해 믿는 CA 인증서(DER). 사설 수집 서버와 시험에 쓴다.
    pub extra_roots: Vec<Vec<u8>>,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(60),
            interval: Duration::from_secs(24 * 3600),
            retry: Duration::from_secs(3600),
            timeout: Duration::from_secs(30),
            extra_roots: Vec::new(),
        }
    }
}

/// 텔레메트리 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 보낸다.
pub struct TelemetryReporter {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TelemetryReporter {
    pub fn spawn(state: Arc<AppState>, config: TelemetryConfig) -> Self {
        let (shutdown, rx) = oneshot::channel();
        Self {
            shutdown,
            task: tokio::spawn(run(state, config, rx)),
        }
    }

    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn run(state: Arc<AppState>, config: TelemetryConfig, mut shutdown: oneshot::Receiver<()>) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut memory = Memory::default();
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        // 보내기와 선거 중의 기록은 오래 걸릴 수 있으므로 종료 요청과 겨룬다.
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut memory) => {}
        }
    }
}

/// 리더의 메모리에만 있는 보내기 기록
#[derive(Debug, Default)]
struct Memory {
    /// 실패한 뒤 다시 보낼 시각
    next_retry: Option<u64>,
    /// 마지막으로 보낸 시각. 보낸 뒤 상태 쓰기가 실패해도 다시 보내지 않게 한다.
    last_sent: Option<u64>,
}

/// 지금 보낼 때인지. 마지막으로 보낸 시각은 설정(`recorded`)과 메모리 중 늦은 것이다.
fn due(now: u64, recorded: Option<u64>, memory: &Memory, interval: Duration) -> bool {
    let last_sent = recorded.max(memory.last_sent);
    last_sent.is_none_or(|t| now >= t.saturating_add(interval.as_secs()))
        && memory.next_retry.is_none_or(|t| now >= t)
}

/// 켜져 있는데 쓸 끝점이 없을 때 적는 오류
const NO_ENDPOINT: &str = "no telemetry endpoint: set telemetry.endpoint";

async fn tick(state: &AppState, config: &TelemetryConfig, memory: &mut Memory) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        return;
    }
    // 옛 노드는 텔레메트리 상태가 든 설정을 풀지 못한다.
    if let Err(e) = crate::cluster::require_current(cluster) {
        tracing::debug!(reason = %e.message, "telemetry waits for the upgrade");
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 설정을 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok(settings) = crate::settings::current(cluster) else {
        return;
    };
    let telemetry = &settings.spec.telemetry;
    if !telemetry.enabled {
        return;
    }
    let endpoint = telemetry
        .endpoint
        .clone()
        .unwrap_or_else(|| TELEMETRY_URL.to_string());
    let status = settings.status.telemetry.unwrap_or_default();
    if endpoint.is_empty() {
        // 같은 오류가 이미 적혀 있으면 쓰지 않는다.
        if status.last_error.as_deref() != Some(NO_ENDPOINT) {
            let written = crate::settings::write_status(cluster, |status| {
                status.telemetry.get_or_insert_default().last_error = Some(NO_ENDPOINT.into());
            })
            .await;
            if let Err(e) = written {
                tracing::warn!(error = %e.message, "cannot record the telemetry result");
            }
        }
        return;
    }
    let now = now();
    if !due(now, status.last_sent_at, memory, config.interval) {
        return;
    }
    // id는 켤 때 따로 적으므로 그 사이에는 아직 없을 수 있다. 그때는 먼저 적고 적힌 값을 쓴다 —
    // 켜는 쪽도 이미 있는 id는 두므로 보고마다 id가 같다.
    let id = match status.id {
        Some(id) => id,
        None => {
            let written = crate::settings::write_status(cluster, |status| {
                status
                    .telemetry
                    .get_or_insert_default()
                    .id
                    .get_or_insert_with(Uuid::new_v4);
            })
            .await;
            match written.map(|s| s.status.telemetry.and_then(|t| t.id)) {
                Ok(Some(id)) => id,
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!(error = %e.message, "cannot record the telemetry id");
                    return;
                }
            }
        }
    };
    let report = match build_report(cluster, id, now).await {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "cannot build the telemetry report");
            return;
        }
    };
    let error = send(config, &endpoint, &report).await.err();
    match &error {
        None => {
            memory.next_retry = None;
            memory.last_sent = Some(now);
            tracing::info!(%endpoint, "telemetry report sent");
        }
        Some(e) => {
            // 보내기가 오래 걸렸을 수 있으므로 retry는 실패한 때부터 센다.
            memory.next_retry = Some(self::now().saturating_add(config.retry.as_secs()));
            tracing::warn!(%endpoint, error = %e, "cannot send the telemetry report");
        }
    }
    let written = crate::settings::write_status(cluster, |status| {
        let telemetry = status.telemetry.get_or_insert_default();
        match &error {
            None => {
                telemetry.last_sent_at = Some(now);
                telemetry.last_error = None;
            }
            Some(e) => telemetry.last_error = Some(e.clone()),
        }
    })
    .await;
    if let Err(e) = written {
        tracing::warn!(error = %e.message, "cannot record the telemetry result");
    }
}

/// 보고를 JSON으로 `endpoint`에 보낸다. 2xx가 아니면 상태 코드가 오류다(3xx도 따르지 않고 오류다 —
/// 응답이 보고를 다른 곳으로 보내지 못하게). https만 쓴다. 프록시는 `HTTPS_PROXY`를 따른다.
async fn send(
    config: &TelemetryConfig,
    endpoint: &str,
    report: &TelemetryReport,
) -> Result<(), String> {
    let mut roots = Vec::new();
    for der in &config.extra_roots {
        roots.push(reqwest::Certificate::from_der(der).map_err(|e| e.to_string())?);
    }
    let client = reqwest::Client::builder()
        .timeout(config.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .tls_certs_merge(roots)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(endpoint)
        .json(report)
        .send()
        .await
        .map_err(|e| chain(&e))?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {status}"))
    }
}

/// 오류와 그 원인들을 `: `로 잇는다(reqwest의 겉 오류만으로는 이유가 보이지 않는다).
pub(crate) fn chain(e: &dyn Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn due_waits_for_the_interval_and_the_retry() {
        let none = Memory::default();
        assert!(due(1_000, None, &none, DAY));
        assert!(!due(1_000, Some(900), &none, DAY));
        assert!(due(900 + 86_400, Some(900), &none, DAY));
        let retry = Memory {
            next_retry: Some(1_100),
            last_sent: None,
        };
        assert!(!due(1_000, None, &retry, DAY));
        assert!(due(1_100, None, &retry, DAY));
    }

    /// 보낸 뒤 상태 쓰기가 실패해 설정에 기록이 없거나 옛 기록만 있어도 메모리의 시각으로 막는다.
    #[test]
    fn a_send_this_leader_remembers_counts_too() {
        let sent = Memory {
            next_retry: None,
            last_sent: Some(1_000),
        };
        assert!(!due(1_500, None, &sent, DAY));
        assert!(!due(1_500, Some(10), &sent, DAY));
        assert!(due(1_000 + 86_400, Some(10), &sent, DAY));
        // 설정이 더 늦으면 그것을 쓴다(다른 리더가 보냄).
        assert!(!due(1_000 + 86_400, Some(2_000), &sent, DAY));
    }
}
