//! Prometheus 끝점 `GET /api/v1/metrics`(계획 60 §10). 텍스트 형식 0.0.4로 **이 노드의 것만** 내보낸다: 이 노드와
//! 이 노드에서 잰 VM의 마지막 5초 점(gauge), 이 노드에서 준비된 풀, 클러스터 값(노드마다 같다). 위트니스는 지표가
//! 없어 클러스터 값만 내보낸다.
//!
//! 인증은 지금의 API 토큰이나 세션이고 Node와 Vm 읽기 권한이 있어야 한다(viewer 토큰이면 된다). 설정
//! `prometheus.enabled`가 꺼져 있으면 404 `not_found`다. 답은 5초 캐시한다(긁는 쪽이 여럿이어도 Raft 목록을 한 번만
//! 읽는다). 레이블은 `node`·`vm_id`·`pool`·`state`·`phase`뿐이고 VM 이름은 `orbivisor_vm_info`에만 붙인다(이름을
//! 바꿔도 시리즈가 끊기지 않게). 값을 모르면 그 줄을 뺀다.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use orbivisor_cluster::types::NodeId;
use orbivisor_types::clock::{Clock, SystemClock};
use orbivisor_types::metrics::{MetricsQuery, MetricsScope, NodeSample, VmSample};
use orbivisor_types::node::NodeSpec;
use orbivisor_types::resource::ResourceSpec;
use orbivisor_types::vm::{VmPhase, VmSpec};
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::ApiError;
use crate::state::AppState;

/// 같은 답을 다시 만들지 않는 시간(측정 간격과 같다)
pub(crate) const PROMETHEUS_CACHE_TTL: Duration = Duration::from_secs(5);
/// 텍스트 형식 0.0.4
pub(crate) const CONTENT_TYPE_TEXT: &str = "text/plain; version=0.0.4; charset=utf-8";
/// 마지막 점이 이보다 오래됐으면 지금 재지 않는 것으로 보고 내보내지 않는다(측정 간격 5초의 세 배).
const FRESH_MS: i64 = 15_000;
const MIB: f64 = 1_048_576.0;

/// 지금 캐시 칸과 그 칸을 연 때. 칸(`OnceCell`)은 TTL마다 새로 열고, 같은 칸을 기다리는 긁기는 한 번 만든 답을 함께
/// 쓴다(리뷰 2 #10: 만료 뒤 몰린 긁기가 저마다 Raft 목록을 읽지 않게). 만들다 실패하거나 끊기면 다음 요청이 채운다.
#[derive(Default)]
pub(crate) struct PrometheusCache(std::sync::Mutex<Option<(Instant, PrometheusSlot)>>);

type PrometheusSlot = Arc<tokio::sync::OnceCell<Arc<str>>>;

impl PrometheusCache {
    /// TTL 안의 답이나 `render`로 만든 답. 바깥 잠금은 칸을 찾을 때만 잡는다.
    pub(crate) async fn get_or_render<F, Fut, E>(
        &self,
        now: Instant,
        render: F,
    ) -> Result<Arc<str>, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Arc<str>, E>>,
    {
        let slot = {
            let mut cache = self.0.lock().unwrap();
            match cache.as_ref() {
                Some((at, slot)) if now.duration_since(*at) < PROMETHEUS_CACHE_TTL => slot.clone(),
                _ => {
                    let slot = PrometheusSlot::default();
                    *cache = Some((now, slot.clone()));
                    slot
                }
            }
        };
        slot.get_or_try_init(render).await.cloned()
    }
}

/// 이 노드의 Prometheus 지표(텍스트 형식 0.0.4, gauge). 이 노드와 이 노드에서 잰 VM의 마지막 5초 점, 이 노드에서
/// 준비된 풀, 클러스터 값(노드 수·VM 수·리더)이다. 노드마다 따로 긁는다. 루트 `cluster.read`와 루트 아래 모든 VM의
/// 읽기(전역 역할이나 propagate 루트 grant)가 필요하고(viewer 토큰이면 된다), 설정 `prometheus.enabled`가 꺼져 있으면 404 `not_found`다. 5초 캐시한다.
#[utoipa::path(get, path = "/metrics", tag = "metrics",
    responses((status = 200, description = "Prometheus text format 0.0.4", body = String,
        content_type = "text/plain; version=0.0.4"),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_prometheus(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Response, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    // 모든 VM의 값이 들어 있다: 루트에서 아래 모두를 읽는 주체(전역 viewer 이상, 그런 토큰)만이다. propagate가 아닌
    // 루트 grant의 `cluster.read`만으로는 못 읽는다(리뷰 1).
    if state.authorizer.builtin()
        && !state.authz.allows(
            &principal,
            orbivisor_types::authz::Privilege::SystemRead,
            orbivisor_types::authz_eval::AuthzTarget::Object {
                kind: orbivisor_types::resource::Kind::Vm,
                id: uuid::Uuid::nil(),
                folder: None,
            },
        )
    {
        return Err(ApiError::forbidden());
    }
    if !crate::settings::current(&state.cluster)?
        .spec
        .prometheus_enabled()
    {
        return Err(ApiError::not_found(
            "the Prometheus endpoint is turned off; turn it on in the settings (prometheus.enabled)",
        ));
    }
    let text = state
        .prometheus_cache
        .get_or_render(Instant::now(), || async {
            Ok::<_, ApiError>(render(&snapshot(&state)?).into())
        })
        .await?;
    Ok(([(CONTENT_TYPE, CONTENT_TYPE_TEXT)], text.to_string()).into_response())
}

/// 한 번 긁을 때 내보내는 값
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    /// 이 노드 이름
    pub node: String,
    /// 이 노드의 마지막 점(위트니스·지표가 없는 노드는 None)
    pub sample: Option<NodeSample>,
    /// 이 노드의 VM(id 순): id, 이름, 마지막 점(지금 재지 않으면 None)
    pub vms: Vec<(Uuid, String, Option<VmSample>)>,
    /// 이 노드에서 준비된 풀(이름 순): 이름, 쓴 바이트, 전체 바이트
    pub pools: Vec<(String, u64, u64)>,
    /// 클러스터의 노드 수: 켜짐, 꺼짐, 유지 보수(유지 보수가 먼저)
    pub nodes: [u64; 3],
    /// 클러스터의 VM 수(단계마다, [`PHASES`] 순)
    pub phases: [u64; PHASES.len()],
    /// 이 노드가 아는 리더가 있고 정족수가 있다.
    pub leader_present: bool,
    /// 이 노드가 Raft 리더다.
    pub is_leader: bool,
    /// 이 노드가 가진 `firing` 경보 수(info, warning, critical). 경보 장부가 없는 노드(위트니스)는 None이다.
    pub alerts_firing: Option<[u64; 3]>,
}

/// VM 단계(레이블 `phase`). 없는 단계도 0으로 내보내 시리즈가 사라지지 않게 한다.
const PHASES: [VmPhase; 6] = [
    VmPhase::Stopped,
    VmPhase::Starting,
    VmPhase::Running,
    VmPhase::Stopping,
    VmPhase::Failed,
    VmPhase::Migrating,
];

fn phase_name(phase: VmPhase) -> &'static str {
    match phase {
        VmPhase::Stopped => "stopped",
        VmPhase::Starting => "starting",
        VmPhase::Running => "running",
        VmPhase::Stopping => "stopping",
        VmPhase::Failed => "failed",
        VmPhase::Migrating => "migrating",
    }
}

/// 클러스터와 이 노드의 지금 값을 모은다.
fn snapshot(state: &AppState) -> Result<Snapshot, ApiError> {
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let vms = state.cluster.list::<VmSpec>()?.1;
    let me = nodes.iter().find(|n| n.id == state.node);
    let since = Some(SystemClock.now_ms() - FRESH_MS);
    let source = state.metrics.as_ref();
    let sample = source.and_then(|s| {
        s.read(&MetricsQuery {
            scope: MetricsScope::Nodes,
            since,
        })
        .node
        .and_then(|series| series.samples.into_iter().last())
    });
    let mut mine: Vec<_> = vms
        .iter()
        .filter(|vm| vm.status.node == Some(state.node))
        .map(|vm| {
            let last = source.and_then(|s| {
                s.read(&MetricsQuery {
                    scope: MetricsScope::Vm { id: vm.id },
                    since,
                })
                .vms
                .into_iter()
                .filter(|series| series.vm == vm.id)
                .flat_map(|series| series.samples)
                .max_by_key(|s| s.t)
            });
            (vm.id, vm.name.clone(), last)
        })
        .collect();
    mine.sort_by_key(|(id, _, _)| *id);
    let mut pools: Vec<_> = me
        .map(|n| {
            n.status
                .pools
                .iter()
                .filter(|(_, p)| p.ready)
                .map(|(name, p)| {
                    (
                        name.clone(),
                        p.total_bytes.saturating_sub(p.available_bytes),
                        p.total_bytes,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    pools.sort();
    let mut counts = [0u64; 3];
    for n in &nodes {
        let i = if n.spec.maintenance {
            2
        } else if n.status.online {
            0
        } else {
            1
        };
        counts[i] += 1;
    }
    let mut phases = [0u64; PHASES.len()];
    for vm in &vms {
        if let Some(i) = PHASES.iter().position(|p| *p == vm.status.phase) {
            phases[i] += 1;
        }
    }
    let health = state.cluster.health();
    Ok(Snapshot {
        node: me.map_or_else(|| state.node.to_string(), |n| n.name.clone()),
        sample,
        vms: mine,
        pools,
        nodes: counts,
        phases,
        leader_present: health.quorum,
        is_leader: health.leader == Some(NodeId(state.node)),
        alerts_firing: crate::alerts::firing_counts(state),
    })
}

/// 레이블 값 이스케이프(`\` → `\\`, `"` → `\"`, 줄바꿈 → `\n`)
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// 지표 하나(같은 이름의 줄들). 줄이 없으면 HELP·TYPE도 쓰지 않는다.
struct Family {
    name: &'static str,
    help: &'static str,
    lines: Vec<(String, f64)>,
}

impl Family {
    fn new(name: &'static str, help: &'static str) -> Self {
        Self {
            name,
            help,
            lines: Vec::new(),
        }
    }

    /// 레이블 `labels`(이름, 값)의 줄. 값을 모르거나 유한하지 않으면 뺀다.
    fn add(&mut self, labels: &[(&str, &str)], value: Option<f64>) {
        let Some(value) = value.filter(|v| v.is_finite()) else {
            return;
        };
        let labels = labels
            .iter()
            .map(|(k, v)| format!("{k}=\"{}\"", escape(v)))
            .collect::<Vec<_>>()
            .join(",");
        self.lines.push((labels, value));
    }

    fn write(&self, out: &mut String) {
        if self.lines.is_empty() {
            return;
        }
        let _ = writeln!(out, "# HELP {} {}", self.name, self.help);
        let _ = writeln!(out, "# TYPE {} gauge", self.name);
        for (labels, value) in &self.lines {
            if labels.is_empty() {
                let _ = writeln!(out, "{} {value}", self.name);
            } else {
                let _ = writeln!(out, "{}{{{labels}}} {value}", self.name);
            }
        }
    }
}

fn mib(v: Option<u64>) -> Option<f64> {
    v.map(|v| v as f64 * MIB)
}

fn ratio(percent: Option<f64>) -> Option<f64> {
    percent.map(|p| p / 100.0)
}

/// 텍스트 형식 0.0.4로 쓴다.
pub(crate) fn render(s: &Snapshot) -> String {
    let mut families = Vec::new();
    let mut f = Family::new(
        "orbivisor_cluster_leader_present",
        "Whether this node knows a cluster leader that has a quorum (1) or not (0).",
    );
    f.add(&[], Some(f64::from(u8::from(s.leader_present))));
    families.push(f);
    let mut f = Family::new(
        "orbivisor_cluster_nodes",
        "Cluster nodes by state (maintenance before online/offline).",
    );
    for (state, n) in ["online", "offline", "maintenance"].iter().zip(s.nodes) {
        f.add(&[("state", state)], Some(n as f64));
    }
    families.push(f);
    let mut f = Family::new("orbivisor_cluster_vms", "Cluster VMs by phase.");
    for (phase, n) in PHASES.iter().zip(s.phases) {
        f.add(&[("phase", phase_name(*phase))], Some(n as f64));
    }
    families.push(f);
    let node = [("node", s.node.as_str())];
    let mut f = Family::new(
        "orbivisor_node_is_raft_leader",
        "Whether this node is the Raft leader (1) or not (0).",
    );
    f.add(&node, Some(f64::from(u8::from(s.is_leader))));
    families.push(f);
    if let Some(counts) = s.alerts_firing {
        let mut f = Family::new(
            "orbivisor_alerts_firing",
            "Firing alerts this node holds (its node alerts, and the leader alerts on the leader), by severity.",
        );
        for (severity, n) in ["info", "warning", "critical"].iter().zip(counts) {
            f.add(&[("severity", severity)], Some(n as f64));
        }
        families.push(f);
    }
    if let Some(n) = &s.sample {
        let rows: [(&'static str, &'static str, Option<f64>); 9] = [
            (
                "orbivisor_node_cpu_usage_ratio",
                "CPU usage of the node over all cores (0-1), last 5 s sample.",
                ratio(n.cpu),
            ),
            (
                "orbivisor_node_memory_used_bytes",
                "Memory used on the node.",
                mib(n.mem_used_mib),
            ),
            (
                "orbivisor_node_memory_total_bytes",
                "Memory of the node.",
                mib(n.mem_total_mib),
            ),
            (
                "orbivisor_node_disk_read_bytes_per_second",
                "Bytes read from the node's physical disks per second.",
                n.disk_read_bps,
            ),
            (
                "orbivisor_node_disk_write_bytes_per_second",
                "Bytes written to the node's physical disks per second.",
                n.disk_write_bps,
            ),
            (
                "orbivisor_node_network_receive_bytes_per_second",
                "Bytes received on the node's physical NICs per second.",
                n.net_rx_bps,
            ),
            (
                "orbivisor_node_network_transmit_bytes_per_second",
                "Bytes sent on the node's physical NICs per second.",
                n.net_tx_bps,
            ),
            (
                "orbivisor_node_fs_used_bytes",
                "Used bytes of the file system holding the data directory.",
                mib(n.fs_used_mib),
            ),
            (
                "orbivisor_node_fs_total_bytes",
                "Size of the file system holding the data directory.",
                mib(n.fs_total_mib),
            ),
        ];
        for (name, help, value) in rows {
            let mut f = Family::new(name, help);
            f.add(&node, value);
            families.push(f);
        }
    }
    let mut used = Family::new(
        "orbivisor_pool_used_bytes",
        "Used bytes of a storage pool ready on this node.",
    );
    let mut total = Family::new(
        "orbivisor_pool_total_bytes",
        "Size of a storage pool ready on this node.",
    );
    for (pool, u, t) in &s.pools {
        used.add(&[("pool", pool)], Some(*u as f64));
        total.add(&[("pool", pool)], Some(*t as f64));
    }
    families.push(used);
    families.push(total);
    let mut info = Family::new(
        "orbivisor_vm_info",
        "VMs placed on this node; joins vm_id to the VM name.",
    );
    type VmValue = fn(&VmSample) -> Option<f64>;
    let vm_rows: [(&'static str, &'static str, VmValue); 8] = [
        (
            "orbivisor_vm_cpu_usage_ratio",
            "CPU usage of the VM relative to its vCPUs (1 = all busy), last 5 s sample.",
            |v| ratio(v.cpu),
        ),
        (
            "orbivisor_vm_memory_rss_bytes",
            "Resident memory of the VM's QEMU process.",
            |v| mib(v.mem_rss_mib),
        ),
        (
            "orbivisor_vm_disk_read_bytes_per_second",
            "Bytes read by the VM from all its disks per second.",
            |v| v.disk_read_bps,
        ),
        (
            "orbivisor_vm_disk_write_bytes_per_second",
            "Bytes written by the VM to all its disks per second.",
            |v| v.disk_write_bps,
        ),
        (
            "orbivisor_vm_disk_read_ops_per_second",
            "Read operations of the VM on all its disks per second.",
            |v| v.disk_read_iops,
        ),
        (
            "orbivisor_vm_disk_write_ops_per_second",
            "Write operations of the VM on all its disks per second.",
            |v| v.disk_write_iops,
        ),
        (
            "orbivisor_vm_network_receive_bytes_per_second",
            "Bytes received by the VM's guest NICs per second.",
            |v| v.net_rx_bps,
        ),
        (
            "orbivisor_vm_network_transmit_bytes_per_second",
            "Bytes sent by the VM's guest NICs per second.",
            |v| v.net_tx_bps,
        ),
    ];
    let mut vm_families: Vec<Family> = vm_rows
        .iter()
        .map(|(name, help, _)| Family::new(name, help))
        .collect();
    for (id, name, sample) in &s.vms {
        let id = id.to_string();
        info.add(
            &[("vm_id", &id), ("vm", name), ("node", &s.node)],
            Some(1.0),
        );
        if let Some(sample) = sample {
            for ((_, _, value), f) in vm_rows.iter().zip(&mut vm_families) {
                f.add(&[("vm_id", &id)], value(sample));
            }
        }
    }
    families.push(info);
    families.extend(vm_families);
    let mut out = String::new();
    for f in &families {
        f.write(&mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_sample() -> NodeSample {
        NodeSample {
            t: 1,
            cpu: Some(25.0),
            mem_used_mib: Some(1024),
            mem_total_mib: Some(4096),
            disk_read_bps: None,
            disk_write_bps: Some(512.5),
            net_rx_bps: Some(0.0),
            net_tx_bps: Some(f64::NAN),
            fs_used_mib: Some(1),
            fs_total_mib: Some(2),
            interfaces: Vec::new(),
        }
    }

    fn vm_sample() -> VmSample {
        VmSample {
            t: 1,
            cpu: Some(150.0),
            mem_rss_mib: Some(300),
            disk_read_bps: Some(1.0),
            disk_write_bps: None,
            disk_read_iops: Some(2.0),
            disk_write_iops: None,
            net_rx_bps: None,
            net_tx_bps: Some(3.0),
        }
    }

    #[test]
    fn renders_text_format_with_low_cardinality_labels() {
        let s = Snapshot {
            node: "node1".into(),
            sample: Some(node_sample()),
            vms: vec![
                (Uuid::from_u128(1), "web \"1\"\\x".into(), Some(vm_sample())),
                (Uuid::from_u128(2), "idle".into(), None),
            ],
            pools: vec![("local".into(), 10, 30)],
            nodes: [2, 1, 1],
            phases: [3, 0, 1, 0, 0, 0],
            leader_present: true,
            is_leader: false,
            alerts_firing: Some([0, 2, 1]),
        };
        let expected = r#"# HELP orbivisor_cluster_leader_present Whether this node knows a cluster leader that has a quorum (1) or not (0).
# TYPE orbivisor_cluster_leader_present gauge
orbivisor_cluster_leader_present 1
# HELP orbivisor_cluster_nodes Cluster nodes by state (maintenance before online/offline).
# TYPE orbivisor_cluster_nodes gauge
orbivisor_cluster_nodes{state="online"} 2
orbivisor_cluster_nodes{state="offline"} 1
orbivisor_cluster_nodes{state="maintenance"} 1
# HELP orbivisor_cluster_vms Cluster VMs by phase.
# TYPE orbivisor_cluster_vms gauge
orbivisor_cluster_vms{phase="stopped"} 3
orbivisor_cluster_vms{phase="starting"} 0
orbivisor_cluster_vms{phase="running"} 1
orbivisor_cluster_vms{phase="stopping"} 0
orbivisor_cluster_vms{phase="failed"} 0
orbivisor_cluster_vms{phase="migrating"} 0
# HELP orbivisor_node_is_raft_leader Whether this node is the Raft leader (1) or not (0).
# TYPE orbivisor_node_is_raft_leader gauge
orbivisor_node_is_raft_leader{node="node1"} 0
# HELP orbivisor_alerts_firing Firing alerts this node holds (its node alerts, and the leader alerts on the leader), by severity.
# TYPE orbivisor_alerts_firing gauge
orbivisor_alerts_firing{severity="info"} 0
orbivisor_alerts_firing{severity="warning"} 2
orbivisor_alerts_firing{severity="critical"} 1
# HELP orbivisor_node_cpu_usage_ratio CPU usage of the node over all cores (0-1), last 5 s sample.
# TYPE orbivisor_node_cpu_usage_ratio gauge
orbivisor_node_cpu_usage_ratio{node="node1"} 0.25
# HELP orbivisor_node_memory_used_bytes Memory used on the node.
# TYPE orbivisor_node_memory_used_bytes gauge
orbivisor_node_memory_used_bytes{node="node1"} 1073741824
# HELP orbivisor_node_memory_total_bytes Memory of the node.
# TYPE orbivisor_node_memory_total_bytes gauge
orbivisor_node_memory_total_bytes{node="node1"} 4294967296
# HELP orbivisor_node_disk_write_bytes_per_second Bytes written to the node's physical disks per second.
# TYPE orbivisor_node_disk_write_bytes_per_second gauge
orbivisor_node_disk_write_bytes_per_second{node="node1"} 512.5
# HELP orbivisor_node_network_receive_bytes_per_second Bytes received on the node's physical NICs per second.
# TYPE orbivisor_node_network_receive_bytes_per_second gauge
orbivisor_node_network_receive_bytes_per_second{node="node1"} 0
# HELP orbivisor_node_fs_used_bytes Used bytes of the file system holding the data directory.
# TYPE orbivisor_node_fs_used_bytes gauge
orbivisor_node_fs_used_bytes{node="node1"} 1048576
# HELP orbivisor_node_fs_total_bytes Size of the file system holding the data directory.
# TYPE orbivisor_node_fs_total_bytes gauge
orbivisor_node_fs_total_bytes{node="node1"} 2097152
# HELP orbivisor_pool_used_bytes Used bytes of a storage pool ready on this node.
# TYPE orbivisor_pool_used_bytes gauge
orbivisor_pool_used_bytes{pool="local"} 10
# HELP orbivisor_pool_total_bytes Size of a storage pool ready on this node.
# TYPE orbivisor_pool_total_bytes gauge
orbivisor_pool_total_bytes{pool="local"} 30
# HELP orbivisor_vm_info VMs placed on this node; joins vm_id to the VM name.
# TYPE orbivisor_vm_info gauge
orbivisor_vm_info{vm_id="00000000-0000-0000-0000-000000000001",vm="web \"1\"\\x",node="node1"} 1
orbivisor_vm_info{vm_id="00000000-0000-0000-0000-000000000002",vm="idle",node="node1"} 1
# HELP orbivisor_vm_cpu_usage_ratio CPU usage of the VM relative to its vCPUs (1 = all busy), last 5 s sample.
# TYPE orbivisor_vm_cpu_usage_ratio gauge
orbivisor_vm_cpu_usage_ratio{vm_id="00000000-0000-0000-0000-000000000001"} 1.5
# HELP orbivisor_vm_memory_rss_bytes Resident memory of the VM's QEMU process.
# TYPE orbivisor_vm_memory_rss_bytes gauge
orbivisor_vm_memory_rss_bytes{vm_id="00000000-0000-0000-0000-000000000001"} 314572800
# HELP orbivisor_vm_disk_read_bytes_per_second Bytes read by the VM from all its disks per second.
# TYPE orbivisor_vm_disk_read_bytes_per_second gauge
orbivisor_vm_disk_read_bytes_per_second{vm_id="00000000-0000-0000-0000-000000000001"} 1
# HELP orbivisor_vm_disk_read_ops_per_second Read operations of the VM on all its disks per second.
# TYPE orbivisor_vm_disk_read_ops_per_second gauge
orbivisor_vm_disk_read_ops_per_second{vm_id="00000000-0000-0000-0000-000000000001"} 2
# HELP orbivisor_vm_network_transmit_bytes_per_second Bytes sent by the VM's guest NICs per second.
# TYPE orbivisor_vm_network_transmit_bytes_per_second gauge
orbivisor_vm_network_transmit_bytes_per_second{vm_id="00000000-0000-0000-0000-000000000001"} 3
"#;
        assert_eq!(render(&s), expected);
    }

    /// 위트니스(지표 없음): 클러스터 값과 리더 여부만
    #[test]
    fn a_node_without_samples_exports_cluster_values_only() {
        let s = Snapshot {
            node: "w".into(),
            leader_present: false,
            is_leader: false,
            ..Snapshot::default()
        };
        let text = render(&s);
        assert!(text.contains("orbivisor_cluster_leader_present 0\n"));
        assert!(text.contains("orbivisor_node_is_raft_leader{node=\"w\"} 0\n"));
        assert!(!text.contains("orbivisor_node_cpu"));
        assert!(!text.contains("orbivisor_vm_"));
        // 경보 장부가 없는 노드(위트니스)는 경보 수를 내지 않는다.
        assert!(!text.contains("orbivisor_alerts_firing"));
        assert!(!text.contains("orbivisor_pool_"));
    }

    /// 만료 뒤 동시에 온 긁기는 한 번만 만들고 그 답을 함께 쓴다. TTL이 지나면 다시 만든다. 실패는 캐시하지 않는다
    /// (리뷰 2 #10).
    #[tokio::test]
    async fn concurrent_scrapes_render_once_per_ttl() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let cache = PrometheusCache::default();
        let renders = AtomicUsize::new(0);
        let render = || async {
            renders.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<Arc<str>, ()>("text".into())
        };
        let now = Instant::now();
        let (a, b, c) = tokio::join!(
            cache.get_or_render(now, render),
            cache.get_or_render(now, render),
            cache.get_or_render(now + Duration::from_secs(1), render),
        );
        assert_eq!(
            (&*a.unwrap(), &*b.unwrap(), &*c.unwrap()),
            ("text", "text", "text")
        );
        assert_eq!(renders.load(Ordering::SeqCst), 1);
        let later = now + PROMETHEUS_CACHE_TTL;
        assert!(
            cache
                .get_or_render(later, || async { Err::<Arc<str>, _>(()) })
                .await
                .is_err()
        );
        cache.get_or_render(later, render).await.unwrap();
        assert_eq!(renders.load(Ordering::SeqCst), 2);
    }
}
