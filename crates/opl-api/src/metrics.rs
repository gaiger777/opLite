//! 사용량 지표 읽기(설계 §3.3). 지표는 노드마다 메모리에 있으므로, 이 노드는 [`AppState::metrics`]로
//! 직접, 다른 노드는 노드 사이 RPC(`ReadMetrics`)로 읽어 합친다. 위트니스는 VM을 돌리지 않아 묻지 않는다.
//! 답하지 않은 노드는 `unreachable`, 지표를 모르는 옛 노드는 `unsupported`다. 꺼진 것으로 기록된 다른
//! 노드는 묻지 않고(연결 제한을 매번 기다리지 않게) `unreachable`이다.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::types::NodeId;
use opl_peer::MetricsReply;
use opl_types::metrics::{
    MetricsQuery, MetricsScope, NodeMetricsResponse, NodeSeries, VmMetricsResponse,
};
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::vm::VmSpec;
use serde::Deserialize;
use utoipa::IntoParams;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::state::AppState;

/// 다른 노드 하나의 지표를 기다리는 최대 시간
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(3);

/// 지표 읽기의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct MetricsParams {
    /// 이 `t`(그 노드 시계의 유닉스 밀리초)보다 뒤의 점만. 없으면 남은 것 모두(1시간)
    pub since: Option<i64>,
}

/// 노드 하나를 읽은 결과
enum NodeRead {
    Reply(MetricsReply),
    /// 지표를 모르는 노드(옛 노드, 지표 소스가 없는 노드)
    Unsupported,
    /// 답하지 않은 노드와 그 이유
    Unreachable(String),
}

/// 노드 하나의 지표. 이 노드면 직접, 다른 노드면 노드 사이 RPC로 읽는다.
async fn read_node(state: &AppState, id: NodeId, addr: &str, query: MetricsQuery) -> NodeRead {
    if id.0 == state.node {
        return match &state.metrics {
            Some(source) => NodeRead::Reply(source.read(&query)),
            None => NodeRead::Unsupported,
        };
    }
    let Some(peers) = &state.peers else {
        return NodeRead::Unreachable("no node-to-node connection".into());
    };
    let read = tokio::time::timeout(
        PEER_READ_TIMEOUT,
        peers.read_metrics(addr, query, PEER_READ_TIMEOUT),
    )
    .await;
    match read {
        Ok(Ok(Some(reply))) => NodeRead::Reply(reply),
        Ok(Ok(None)) => NodeRead::Unsupported,
        Ok(Err(status)) => NodeRead::Unreachable(status.message().to_string()),
        Err(_) => NodeRead::Unreachable("timed out".into()),
    }
}

/// 위트니스를 뺀 모든 멤버에게 `query`를 묻는다(동시에 `FAN_OUT`개까지). (노드 id, 이름, 결과)를 돌려준다. Node 리소스가 없는
/// 멤버는 id를 이름으로 쓴다. 꺼진 것으로 기록된 다른 노드는 묻지 않고 닿지 않는 노드로 둔다.
async fn read_all(
    state: &AppState,
    query: MetricsQuery,
) -> Result<Vec<(NodeId, String, NodeRead)>, ApiError> {
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let members = state.cluster.members();
    let reads = members
        .voters
        .iter()
        .chain(&members.learners)
        .chain(&members.observers)
        .filter_map(|(id, addr)| {
            let node = nodes.iter().find(|n| n.id == id.0);
            if node.is_some_and(|n| n.spec.witness) {
                return None;
            }
            let name = node.map_or_else(|| id.0.to_string(), |n| n.name.clone());
            let offline = id.0 != state.node && node.is_some_and(|n| !n.status.online);
            let query = query.clone();
            Some(async move {
                let read = if offline {
                    NodeRead::Unreachable("offline".into())
                } else {
                    read_node(state, *id, addr, query).await
                };
                (*id, name, read)
            })
        });
    Ok(crate::fan_out::gather(reads.collect()).await)
}

/// 닿지 않거나 옛 노드면 이름을 목록에 넣고 None, 읽었으면 그 응답
fn sort_out(
    name: String,
    read: NodeRead,
    unreachable: &mut Vec<String>,
    unsupported: &mut Vec<String>,
) -> Option<MetricsReply> {
    match read {
        NodeRead::Reply(reply) => Some(reply),
        NodeRead::Unsupported => {
            unsupported.push(name);
            None
        }
        NodeRead::Unreachable(e) => {
            // 폴링마다 부르므로 경고를 쌓지 않는다.
            tracing::debug!(node = %name, error = %e, "cannot read the metrics of a node");
            unreachable.push(name);
            None
        }
    }
}

/// 모든 노드(위트니스 제외)의 노드 지표. 답하지 않은 노드는 `unreachable`, 지표를 모르는 옛 노드는
/// `unsupported`에 이름이 들어간다.
#[utoipa::path(get, path = "/metrics/nodes", tag = "metrics",
    params(MetricsParams),
    responses((status = 200, body = NodeMetricsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_node_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<MetricsParams>,
) -> Result<Json<NodeMetricsResponse>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: params.since,
    };
    let mut nodes = Vec::new();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    for (id, name, read) in read_all(&state, query).await? {
        if let Some(reply) = sort_out(name.clone(), read, &mut unreachable, &mut unsupported) {
            let samples = reply.node.map(|s| s.samples).unwrap_or_default();
            nodes.push((
                name,
                NodeSeries {
                    node: id.0,
                    samples,
                },
            ));
        }
    }
    nodes.sort_by(|a, b| a.0.cmp(&b.0));
    unreachable.sort();
    unsupported.sort();
    Ok(Json(NodeMetricsResponse {
        nodes: nodes.into_iter().map(|(_, series)| series).collect(),
        unreachable,
        unsupported,
    }))
}

/// 노드 하나의 지표. 위트니스면 409 `node_unavailable`, 꺼진 것으로 기록됐거나 닿지 않으면 503
/// `node_unreachable`, 지표를 모르는 옛 노드면 409 `cluster_upgrading`.
#[utoipa::path(get, path = "/nodes/{key}/metrics", tag = "metrics",
    params(("key" = String, Path, description = "node id or name"), MetricsParams),
    responses((status = 200, body = NodeSeries), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_node_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<MetricsParams>,
) -> Result<Json<NodeSeries>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is a witness node and runs no workloads",
                node.name
            ),
        ));
    }
    let unreachable = |why: &str| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("node {:?} did not answer: {why}", node.name),
        )
    };
    let members = state.cluster.members();
    let Some((id, addr)) = members
        .voters
        .iter()
        .chain(&members.learners)
        .chain(&members.observers)
        .find(|(id, _)| id.0 == node.id)
    else {
        return Err(unreachable("not a cluster member"));
    };
    if node.id != state.node && !node.status.online {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("node {:?} is offline", node.name),
        ));
    }
    let query = MetricsQuery {
        scope: MetricsScope::Nodes,
        since: params.since,
    };
    match read_node(&state, *id, addr, query).await {
        NodeRead::Reply(reply) => Ok(Json(NodeSeries {
            node: node.id,
            samples: reply.node.map(|s| s.samples).unwrap_or_default(),
        })),
        NodeRead::Unsupported => Err(ApiError::conflict(
            "cluster_upgrading",
            format!("node {:?} does not report metrics yet", node.name),
        )),
        NodeRead::Unreachable(e) => Err(unreachable(&e)),
    }
}

/// VM 하나의 지표. 모든 노드(위트니스 제외)에 물어 시각 순으로 합치므로 마이그레이션 앞뒤 기록이
/// 이어진다. 같은 `t`가 두 노드에서 오면 둘 다 둔다.
#[utoipa::path(get, path = "/vms/{key}/metrics", tag = "metrics",
    params(("key" = String, Path, description = "VM id or name"), MetricsParams),
    responses((status = 200, body = VmMetricsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_vm_metrics(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<MetricsParams>,
) -> Result<Json<VmMetricsResponse>, ApiError> {
    state.authorize(&principal, Action::Read, VmSpec::KIND)?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let query = MetricsQuery {
        scope: MetricsScope::Vm { id: vm.id },
        since: params.since,
    };
    let mut samples = Vec::new();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    for (_, name, read) in read_all(&state, query).await? {
        if let Some(reply) = sort_out(name, read, &mut unreachable, &mut unsupported) {
            samples.extend(
                reply
                    .vms
                    .into_iter()
                    .filter(|s| s.vm == vm.id)
                    .flat_map(|s| s.samples),
            );
        }
    }
    // 안정 정렬이라 같은 `t`는 둘 다 남는다.
    samples.sort_by_key(|s| s.t);
    unreachable.sort();
    unsupported.sort();
    Ok(Json(VmMetricsResponse {
        vm: vm.id,
        samples,
        unreachable,
        unsupported,
    }))
}
