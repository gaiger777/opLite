//! 지표 기록 조회(계획 60 §7·§12): 노드·VM·풀 하나의 기록과 클러스터 모으기.
//!
//! 기록은 노드마다 파일에 있으므로 한 곳에만 묻는다. 노드 기록은 그 노드, VM 기록은 VM의 주인 노드(`status.node`, 없으면
//! `spec.node`), 풀 기록은 그 풀이 `ready`인 노드 하나(요청 노드 먼저, 없으면 이름 순)다. 이 노드면
//! [`AppState::metrics`]를 막히는 스레드에서, 다른 노드면 노드 사이 RPC `ReadMetricsHistory`로 읽는다. 백분위는 기록
//! 노드가 셈한다(지난 1시간은 5초 원본으로 정확히, 그 밖은 층의 AVG로 근사하고 `approx`).
//!
//! 클러스터 모으기(`GET /metrics/cluster`)는 위트니스를 뺀 노드에 `Digest`를 [`crate::fan_out::gather`]로 묻고(동시에
//! 32개까지), 같은 층·칸으로 답한 노드의 열을 합친다(`cluster_sum`). 같은 조건은 이 API 노드에서 10초 캐시하고, 같은
//! 조건을 채우는 동안 온 요청은 그것을 기다린다(모으기가 겹쳐 노드에 묻는 수가 늘지 않게). 다른 조건은 따로 돈다.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use orbivisor_cluster::types::NodeId;
use orbivisor_peer::HistoryError;
use orbivisor_types::clock::{Clock, SystemClock};
use orbivisor_types::metrics::{
    ClusterHistoryResponse, ClusterNodeRow, ClusterPoolUsage, ClusterTopVm, DEFAULT_HISTORY_POINTS,
    DIGEST_POINTS, DIGEST_TOP, HistoryQuery, HistoryRange, HistoryReply, HistoryScope,
    MAX_CLUSTER_NODE_ROWS, MetricField, Tier,
};
use orbivisor_types::metrics_math::{
    bucket_window, choose_tier, cluster_sum, group_size, merge_top, stats,
};
use orbivisor_types::node::{METRICS_HISTORY, Node, NodeSpec};
use orbivisor_types::resource::ResourceSpec;
use orbivisor_types::storage::{StoragePool, StoragePoolSpec, is_shared};
use orbivisor_types::vm::{Vm, VmSpec};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::state::AppState;

/// 다른 노드의 기록 하나를 기다리는 최대 시간(파일을 읽으므로 1시간 지표 읽기 3초보다 길다)
const HISTORY_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 같은 조건의 클러스터 모으기를 다시 묻지 않는 시간
pub(crate) const DIGEST_CACHE_TTL: Duration = Duration::from_secs(10);
/// 캐시에 두는 조건 수(`from`·`until`을 바꿔 가며 물어도 캐시가 끝없이 크지 않게)
const DIGEST_CACHE_MAX: usize = 64;

/// 기록 조회의 범위와 점 수. `range`이거나 `from`(과 `until`)이고, 둘 다 없으면 지난 1시간이다.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct HistoryParams {
    /// 지금에서 끝나는 범위: `hour`, `day`, `week`, `month`(30일), `year`(365일). `from`·`until`과 함께 쓰지 않는다.
    #[param(inline)]
    pub range: Option<HistoryRange>,
    /// 범위 시작(유닉스 밀리초)
    pub from: Option<i64>,
    /// 범위 끝(유닉스 밀리초, 없으면 지금). `from`과 함께만 쓴다. 범위는 366일까지다.
    pub until: Option<i64>,
    /// 돌려줄 점 수의 상한(1~1000). 기본은 300이고 클러스터 모으기는 120이다.
    #[param(minimum = 1, maximum = 1000)]
    pub points: Option<u16>,
}

impl HistoryParams {
    /// `now`(이 노드 시계)를 기준으로 조회를 만들고 검사한다. 맞지 않으면 400 `invalid_request`다.
    fn query(
        &self,
        scope: HistoryScope,
        now: i64,
        default_points: u16,
    ) -> Result<HistoryQuery, ApiError> {
        let (from, until) = match (self.range, self.from, self.until) {
            (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                return Err(ApiError::bad_request(
                    "give either range or from/until, not both",
                ));
            }
            (None, None, Some(_)) => {
                return Err(ApiError::bad_request("until needs from"));
            }
            (Some(range), None, None) => range.window(now),
            (None, Some(from), until) => (from, until.unwrap_or(now)),
            (None, None, None) => HistoryRange::Hour.window(now),
        };
        let query = HistoryQuery {
            scope,
            from,
            until,
            points: self.points.unwrap_or(default_points),
        };
        query
            .validate()
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        Ok(query)
    }

    /// 클러스터 모으기 캐시의 열쇠: 받은 조건 그대로(`range`면 창이 지금과 함께 움직여도 같은 열쇠다)
    fn cache_key(&self, points: u16) -> DigestKey {
        (
            self.range.map(HistoryRange::as_str),
            self.from,
            self.until,
            points,
        )
    }
}

/// 노드 `node`의 기록을 읽는다. 그 노드가 `metrics-history`를 알리지 않으면 묻지 않고 `Unsupported`, 이 노드가 아니고
/// 꺼진 것으로 기록됐으면 묻지 않고 `Unreachable`이다.
async fn read_history(
    state: &AppState,
    node: &Node,
    query: HistoryQuery,
) -> Result<HistoryReply, HistoryError> {
    if !node.status.features.iter().any(|f| f == METRICS_HISTORY) {
        return Err(HistoryError::Unsupported);
    }
    if node.id == state.node {
        let Some(source) = state.metrics.clone() else {
            return Err(HistoryError::Unsupported);
        };
        return tokio::task::spawn_blocking(move || source.history(&query))
            .await
            .map_err(|e| HistoryError::Io(e.to_string()))?;
    }
    if !node.status.online {
        return Err(HistoryError::Unreachable("the node is offline".into()));
    }
    let Some(peers) = &state.peers else {
        return Err(HistoryError::Unreachable(
            "no node-to-node connection".into(),
        ));
    };
    let Some(addr) = state
        .cluster
        .members()
        .addr(NodeId(node.id))
        .map(str::to_string)
    else {
        return Err(HistoryError::Unreachable("not a cluster member".into()));
    };
    tokio::time::timeout(
        HISTORY_READ_TIMEOUT,
        peers.read_metrics_history(NodeId(node.id), &addr, query, HISTORY_READ_TIMEOUT),
    )
    .await
    .map_err(|_| HistoryError::Unreachable("timed out".into()))?
}

/// 기록을 읽지 못한 이유를 API 오류로 바꾼다: 옛 노드는 409 `cluster_upgrading`, 닿지 않으면 503
/// `node_unreachable`, 잘못된 조회는 400, 파일 오류는 500.
fn history_error(node: &str, e: HistoryError) -> ApiError {
    match e {
        HistoryError::Unsupported => ApiError::conflict(
            "cluster_upgrading",
            format!("node {node:?} does not keep metrics history yet; upgrade it first"),
        ),
        HistoryError::Unreachable(why) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("node {node:?} did not answer: {why}"),
        ),
        HistoryError::Invalid(why) => ApiError::bad_request(why),
        HistoryError::Io(_) | HistoryError::Disabled => {
            ApiError::internal(format!("metrics history of node {node:?}: {e}"))
        }
    }
}

fn now_ms() -> i64 {
    SystemClock.now_ms()
}

/// 노드 하나의 기록(노드 열 9개). 위트니스면 409 `node_unavailable`, 기록을 모르는 옛 노드면 409
/// `cluster_upgrading`, 꺼진 것으로 기록됐거나 닿지 않으면 503 `node_unreachable`이다.
#[utoipa::path(get, path = "/nodes/{key}/metrics/history", tag = "metrics",
    params(("key" = String, Path, description = "node id or name"), HistoryParams),
    responses((status = 200, body = HistoryReply), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_node_history(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<HistoryReply>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!(
                "node {:?} is a witness node and keeps no metrics history",
                node.name
            ),
        ));
    }
    let query = params.query(HistoryScope::Node, now_ms(), DEFAULT_HISTORY_POINTS)?;
    read_history(&state, &node, query)
        .await
        .map(Json)
        .map_err(|e| history_error(&node.name, e))
}

/// VM 하나의 기록(VM 열 8개). VM의 주인 노드(`status.node`, 없으면 `spec.node`, 둘 다 없으면 이 노드)에만 묻는다.
/// 마이그레이션 직후 앞 기록이 넘어오기 전이면 앞 칸이 `null`이고 `covered_from`이 알린다. 주인 노드가 기록을 모르면
/// 409 `cluster_upgrading`, 닿지 않으면 503 `node_unreachable`이다.
#[utoipa::path(get, path = "/vms/{key}/metrics/history", tag = "metrics",
    params(("key" = String, Path, description = "VM id or name"), HistoryParams),
    responses((status = 200, body = HistoryReply), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_vm_history(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<HistoryReply>, ApiError> {
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let query = params.query(
        HistoryScope::Vm { id: vm.id },
        now_ms(),
        DEFAULT_HISTORY_POINTS,
    )?;
    let owner = owner(&vm).unwrap_or(state.node);
    let Some(node) = state.cluster.get::<NodeSpec>(owner)? else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("the node of VM {:?} is not in the cluster", vm.name),
        ));
    };
    read_history(&state, &node, query)
        .await
        .map(Json)
        .map_err(|e| history_error(&node.name, e))
}

/// VM 기록의 주인 노드
fn owner(vm: &Vm) -> Option<Uuid> {
    vm.status.node.or(vm.spec.node)
}

/// 풀 `pool`이 `ready`인 노드(위트니스 제외): 이 노드 먼저, 그다음 이름 순
fn ready_nodes<'a>(pool: &StoragePool, nodes: &'a [Node], me: Uuid) -> Vec<&'a Node> {
    let mut ready: Vec<&Node> = nodes
        .iter()
        .filter(|n| !n.spec.witness)
        .filter(|n| {
            n.status
                .pools
                .get(&pool.name)
                .is_some_and(|s| s.ready && s.pool_id == pool.id)
        })
        .collect();
    ready.sort_by(|a, b| (a.id != me).cmp(&(b.id != me)).then(a.name.cmp(&b.name)));
    ready
}

/// 풀 하나의 기록(`pool_used`·`pool_total`, MiB). 그 풀이 `ready`인 노드 하나(이 노드 먼저, 없으면 이름 순으로 켜져 있고
/// 기록을 아는 노드)에 묻는다. 노드 로컬 풀이면 그 노드의 값이다. 준비된 노드가 없으면 409 `pool_unavailable`, 모두
/// 옛 노드면 409 `cluster_upgrading`, 모두 꺼져 있거나 닿지 않으면 503 `node_unreachable`이다.
#[utoipa::path(get, path = "/storage-pools/{key}/metrics/history", tag = "metrics",
    params(("key" = String, Path, description = "pool id or name"), HistoryParams),
    responses((status = 200, body = HistoryReply), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_pool_history(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<HistoryParams>,
) -> Result<Json<HistoryReply>, ApiError> {
    let pool = lookup::<StoragePoolSpec>(&state.cluster, &key)?;
    let query = params.query(
        HistoryScope::Pool { id: pool.id },
        now_ms(),
        DEFAULT_HISTORY_POINTS,
    )?;
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let ready = ready_nodes(&pool, &nodes, state.node);
    if ready.is_empty() {
        return Err(ApiError::conflict(
            "pool_unavailable",
            format!("pool {:?} is not ready on any node", pool.name),
        ));
    }
    let knows = |n: &&&Node| n.status.features.iter().any(|f| f == METRICS_HISTORY);
    // 기록을 아는 노드 가운데 켜진 첫 노드, 없으면 기록을 아는 첫 노드(503), 없으면 첫 노드(409)
    let node = ready
        .iter()
        .filter(knows)
        .find(|n| n.id == state.node || n.status.online)
        .or_else(|| ready.iter().find(knows))
        .unwrap_or(&ready[0]);
    read_history(&state, node, query)
        .await
        .map(Json)
        .map_err(|e| history_error(&node.name, e))
}

/// 클러스터 모으기 캐시의 열쇠: (`range`, `from`, `until`, `points`)
type DigestKey = (Option<&'static str>, Option<i64>, Option<i64>, u16);

/// 조건 하나의 캐시 칸: 비었으면 누군가 채우는 중이고, 채워졌으면 (채운 때, 답)이다.
type DigestSlot = Arc<tokio::sync::OnceCell<(Instant, Arc<ClusterHistoryResponse>)>>;

/// 클러스터 모으기의 캐시(이 API 노드 메모리). 바깥 잠금은 칸을 찾을 때만 잡고, 모으기는 칸마다 한 번만 돈다.
#[derive(Default)]
pub(crate) struct DigestCache {
    entries: HashMap<DigestKey, DigestSlot>,
}

impl DigestCache {
    /// 조건의 칸: 채우는 중이거나 아직 새것이면 그 칸, 아니면 새 빈 칸(오래된 칸은 정리하고, 많으면 가장 오래 채운
    /// 칸을 뺀다).
    fn slot(&mut self, key: DigestKey, now: Instant) -> DigestSlot {
        let fresh = |s: &DigestSlot| {
            s.get()
                .is_none_or(|(at, _)| now.duration_since(*at) < DIGEST_CACHE_TTL)
        };
        if let Some(s) = self.entries.get(&key)
            && fresh(s)
        {
            return s.clone();
        }
        self.entries.retain(|_, s| fresh(s));
        if self.entries.len() >= DIGEST_CACHE_MAX
            && let Some(oldest) = (self.entries.iter())
                .min_by_key(|(_, s)| s.get().map_or(now, |(at, _)| *at))
                .map(|(k, _)| *k)
        {
            self.entries.remove(&oldest);
        }
        let s = DigestSlot::default();
        self.entries.insert(key, s.clone());
        s
    }
}

/// 조건 `key`의 답을 캐시에서 주거나 `fill`로 채운다. 같은 조건을 채우는 중이면 그것을 기다리고(채우던 요청이
/// 끊기거나 실패하면 기다리던 요청 하나가 이어 채운다), 다른 조건은 기다리지 않는다.
pub(crate) async fn cached_digest<F, Fut>(
    cache: &std::sync::Mutex<DigestCache>,
    key: DigestKey,
    fill: F,
) -> Result<Arc<ClusterHistoryResponse>, ApiError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ClusterHistoryResponse, ApiError>>,
{
    let slot = cache.lock().unwrap().slot(key, Instant::now());
    let (_, response) = slot
        .get_or_try_init(|| async { Ok::<_, ApiError>((Instant::now(), Arc::new(fill().await?))) })
        .await?;
    Ok(response.clone())
}

/// 클러스터 전체의 기록 요약: 노드 열을 합친 클러스터 열(CPU %는 코어 수로 가중, 나머지는 합, AVG만), 노드 줄(20개
/// 넘으면 범위 평균 CPU 상위 20개), 범위 평균 CPU 상위 VM 10개, 풀의 지금 사용량(Raft). 답하지 않은 노드는
/// `unreachable`, 기록을 모르는 옛 노드는 `unsupported`, 시계가 크게 어긋나 다른 층·칸으로 답한 노드는 `skewed`다.
/// 같은 조건은 이 노드에서 10초 동안 다시 묻지 않는다.
#[utoipa::path(get, path = "/metrics/cluster", tag = "metrics",
    params(HistoryParams),
    responses((status = 200, body = ClusterHistoryResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cluster_history(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<HistoryParams>,
) -> Result<Json<ClusterHistoryResponse>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let now = now_ms();
    let query = params.query(HistoryScope::Digest { top: DIGEST_TOP }, now, DIGEST_POINTS)?;
    let key = params.cache_key(query.points);
    let response = cached_digest(&state.digest_cache, key, || digest(&state, &query, now)).await?;
    let mut response = response.as_ref().clone();
    // 상위 VM과 풀은 볼 수 있는 것만 낸다(캐시는 모든 주체가 함께 쓴다, 계획 62).
    let owners = crate::authz::Owners::load(&state.cluster)?;
    response.top = state.authz.filter_by(&principal, response.top, |t| {
        crate::authz::Owned::object(owners.vm(t.vm))
    })?;
    let pools = state.cluster.list::<StoragePoolSpec>()?.1;
    response.pools = state.authz.filter_by(&principal, response.pools, |p| {
        crate::authz::Owned::object(orbivisor_types::authz_eval::AuthzTarget::Object {
            kind: orbivisor_types::resource::Kind::StoragePool,
            id: p.pool,
            folder: pools
                .iter()
                .find(|x| x.id == p.pool)
                .and_then(|x| x.spec.folder),
        })
    })?;
    Ok(Json(response))
}

/// 노드 하나의 `Digest` 답
pub(crate) struct NodeAnswer {
    pub id: Uuid,
    pub name: String,
    /// `NodeStatus.cpus`(CPU 열의 무게)
    pub cpus: u32,
    pub reply: Result<HistoryReply, HistoryError>,
}

/// 위트니스를 뺀 노드에 `query`를 묻고 합친다.
async fn digest(
    state: &AppState,
    query: &HistoryQuery,
    now: i64,
) -> Result<ClusterHistoryResponse, ApiError> {
    let nodes: Vec<Node> = state
        .cluster
        .list::<NodeSpec>()?
        .1
        .into_iter()
        .filter(|n| !n.spec.witness)
        .collect();
    let answers = crate::fan_out::gather(
        nodes
            .iter()
            .map(|node| async move {
                NodeAnswer {
                    id: node.id,
                    name: node.name.clone(),
                    cpus: node.status.cpus,
                    reply: read_history(state, node, query.clone()).await,
                }
            })
            .collect(),
    )
    .await;
    let vms = state.cluster.list::<VmSpec>()?.1;
    let pools = state.cluster.list::<StoragePoolSpec>()?.1;
    Ok(merge_digest(
        state.node, query, now, answers, &vms, &nodes, &pools,
    ))
}

/// 노드들의 `Digest` 답을 합친다. 답한 노드 가운데 가장 많은 노드가 쓴 `(층, 시작, 간격)`(같으면 이 노드 `me`의 것,
/// 그다음 고운 층)을 기준으로 하고, 다른 노드는 `skewed`로 빼 열·줄에 넣지 않는다(층은 기록 노드의 시계로 고르므로
/// 시계가 크게 어긋난 노드만 다르다). 상위 VM은 모든 답에서 합쳐 Raft에 없는 VM을 뺀다.
pub(crate) fn merge_digest(
    me: Uuid,
    query: &HistoryQuery,
    now: i64,
    answers: Vec<NodeAnswer>,
    vms: &[Vm],
    nodes: &[Node],
    pools: &[StoragePool],
) -> ClusterHistoryResponse {
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    let mut answered: Vec<(Uuid, String, u32, HistoryReply)> = Vec::new();
    for a in answers {
        match a.reply {
            Ok(reply) => answered.push((a.id, a.name, a.cpus, reply)),
            Err(HistoryError::Unsupported) => unsupported.push(a.name),
            Err(e) => {
                // 대시보드가 폴링하므로 경고를 쌓지 않는다.
                tracing::debug!(node = %a.name, error = %e, "cannot read the metrics history of a node");
                unreachable.push(a.name);
            }
        }
    }
    let grid = |r: &HistoryReply| (r.tier, r.start, r.step_ms);
    let mut counts: BTreeMap<(Tier, i64, i64), (usize, bool)> = BTreeMap::new();
    for (id, _, _, reply) in &answered {
        let c = counts.entry(grid(reply)).or_default();
        c.0 += 1;
        c.1 |= *id == me;
    }
    // 노드 수, 이 노드의 것, 고운 층(BTreeMap 순서가 고운 층부터라 같으면 앞의 것)
    let reference = counts
        .iter()
        .rev()
        .max_by_key(|(_, (n, mine))| (*n, *mine))
        .map(|(g, _)| *g)
        .unwrap_or_else(|| empty_grid(query, now));
    let mut skewed = Vec::new();
    let mut aligned = Vec::new();
    let mut tops = Vec::new();
    for (id, name, cpus, mut reply) in answered {
        tops.push(std::mem::take(&mut reply.top));
        if grid(&reply) == reference {
            aligned.push((id, name, cpus, reply));
        } else {
            skewed.push(name);
        }
    }
    let columns = cluster_sum(aligned.iter().map(|(_, _, cpus, r)| (*cpus, &r.columns)));
    let cpu_name = MetricField::Cpu.as_str();
    let mut rows: Vec<ClusterNodeRow> = aligned
        .into_iter()
        .map(|(id, name, cpus, reply)| ClusterNodeRow {
            node: id,
            name,
            cpus,
            avg: reply
                .columns
                .iter()
                .map(|(field, cf)| (field.clone(), stats(cf).avg))
                .collect(),
            cpu: reply
                .columns
                .get(cpu_name)
                .map(|cf| cf.avg.clone())
                .unwrap_or_default(),
        })
        .collect();
    let node_count = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    if rows.len() > MAX_CLUSTER_NODE_ROWS {
        let cpu = |r: &ClusterNodeRow| {
            r.avg
                .get(cpu_name)
                .copied()
                .flatten()
                .unwrap_or(f32::NEG_INFINITY)
        };
        rows.sort_by(|a, b| cpu(b).total_cmp(&cpu(a)).then(a.name.cmp(&b.name)));
        rows.truncate(MAX_CLUSTER_NODE_ROWS);
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    let vm_by_id: BTreeMap<Uuid, &Vm> = vms.iter().map(|vm| (vm.id, vm)).collect();
    let node_name: BTreeMap<Uuid, &str> = nodes.iter().map(|n| (n.id, n.name.as_str())).collect();
    let top = merge_top(tops, usize::MAX)
        .into_iter()
        .filter_map(|t| {
            let vm = vm_by_id.get(&t.vm)?;
            Some(ClusterTopVm {
                vm: t.vm,
                name: vm.name.clone(),
                node: owner(vm)
                    .and_then(|n| node_name.get(&n))
                    .map(|n| n.to_string()),
                cpu: t.cpu,
                mem_rss: t.mem_rss,
            })
        })
        .take(usize::from(DIGEST_TOP))
        .collect();
    unreachable.sort();
    unsupported.sort();
    skewed.sort();
    let (tier, start, step_ms) = reference;
    ClusterHistoryResponse {
        tier,
        step_ms,
        start,
        columns,
        nodes: rows,
        node_count,
        top,
        pools: pool_usage(pools, nodes),
        unreachable,
        unsupported,
        skewed,
    }
}

/// 아무 노드도 답하지 않았을 때 이 노드 시계로 셈한 `(층, 시작, 간격)`(기록 노드가 셈하는 것과 같다)
fn empty_grid(query: &HistoryQuery, now: i64) -> (Tier, i64, i64) {
    let tier = choose_tier(query.from, query.until, now);
    let (first, count) = bucket_window(tier, query.from, query.until);
    let group = group_size(count, query.points);
    let step = tier.step_ms();
    (tier, first * step, step * group as i64)
}

/// 풀의 지금 사용량(Raft `NodeStatus.pools`, 1 GiB 넘게 바뀔 때만 쓰인 값). 공유 풀은 준비된 첫 노드(이름 순)의 값,
/// 노드 로컬 풀은 준비된 노드 값의 합이다. 준비된 노드가 없는 풀은 뺀다.
fn pool_usage(pools: &[StoragePool], nodes: &[Node]) -> Vec<ClusterPoolUsage> {
    let mut nodes: Vec<&Node> = nodes.iter().collect();
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out: Vec<ClusterPoolUsage> = pools
        .iter()
        .filter_map(|pool| {
            let shared = is_shared(pool);
            let mut states = nodes.iter().filter_map(|n| {
                n.status
                    .pools
                    .get(&pool.name)
                    .filter(|s| s.ready && s.pool_id == pool.id)
            });
            let (used, total) = if shared {
                let s = states.next()?;
                (
                    s.total_bytes.saturating_sub(s.available_bytes),
                    s.total_bytes,
                )
            } else {
                let mut any = false;
                let sum = states.fold((0u64, 0u64), |(used, total), s| {
                    any = true;
                    (
                        used.saturating_add(s.total_bytes.saturating_sub(s.available_bytes)),
                        total.saturating_add(s.total_bytes),
                    )
                });
                if !any {
                    return None;
                }
                sum
            };
            Some(ClusterPoolUsage {
                pool: pool.id,
                name: pool.name.clone(),
                shared,
                used_bytes: used,
                total_bytes: total,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use orbivisor_types::metrics::{Cf, TopVm};
    use orbivisor_types::node::{NodeStatus, PoolState};
    use orbivisor_types::resource::Resource;
    use orbivisor_types::storage::PoolBackend;

    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const HOUR: i64 = 3_600_000;

    fn digest_query() -> HistoryQuery {
        HistoryQuery {
            scope: HistoryScope::Digest { top: DIGEST_TOP },
            from: NOW - HOUR,
            until: NOW,
            points: DIGEST_POINTS,
        }
    }

    fn reply(tier: Tier, start: i64, cpu: Vec<Option<f32>>, top: Vec<TopVm>) -> HistoryReply {
        let mem = cpu.iter().map(|c| c.map(|_| 1024.0)).collect();
        HistoryReply {
            tier,
            step_ms: 30_000,
            start,
            columns: BTreeMap::from([
                (
                    "cpu".to_string(),
                    Cf {
                        avg: cpu,
                        ..Cf::default()
                    },
                ),
                (
                    "mem_used".to_string(),
                    Cf {
                        avg: mem,
                        ..Cf::default()
                    },
                ),
            ]),
            stats: BTreeMap::new(),
            pct: BTreeMap::new(),
            covered_from: None,
            top,
        }
    }

    fn answer(n: u128, cpus: u32, reply: Result<HistoryReply, HistoryError>) -> NodeAnswer {
        NodeAnswer {
            id: Uuid::from_u128(n),
            name: format!("node{n}"),
            cpus,
            reply,
        }
    }

    fn vm(n: u128, node: u128) -> Vm {
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 },
            "memory_mib": 256,
            "machine": "virt",
        }))
        .unwrap();
        let mut vm: Vm = Resource::new(format!("vm{n}"), spec);
        vm.id = Uuid::from_u128(1000 + n);
        vm.status.node = Some(Uuid::from_u128(node));
        vm
    }

    fn top(n: u128, cpu: f32) -> TopVm {
        TopVm {
            vm: Uuid::from_u128(1000 + n),
            cpu: Some(cpu),
            mem_rss: Some(100.0),
        }
    }

    fn node(n: u128, pools: &[(&str, Uuid, bool, u64, u64)]) -> Node {
        let mut r: Node = Resource::new(
            format!("node{n}"),
            NodeSpec {
                sdn: None,
                host_resources: None,
                address: String::new(),
                maintenance: false,
                witness: false,
                roles: None,
            },
        );
        r.id = Uuid::from_u128(n);
        r.status = NodeStatus {
            pools: pools
                .iter()
                .map(|(name, id, ready, total, avail)| {
                    (
                        name.to_string(),
                        PoolState {
                            pool_id: *id,
                            ready: *ready,
                            error: None,
                            total_bytes: *total,
                            available_bytes: *avail,
                        },
                    )
                })
                .collect(),
            ..NodeStatus::default()
        };
        r
    }

    fn dir_pool(shared: bool) -> StoragePoolSpec {
        StoragePoolSpec {
            backend: PoolBackend::Dir {
                path: "/srv/pool".into(),
            },
            nodes: vec![],
            shared,
            backups: false,
            folder: None,
        }
    }

    #[test]
    fn params_make_a_window_or_say_why_not() {
        let p = |range, from, until, points| HistoryParams {
            range,
            from,
            until,
            points,
        };
        let q = p(None, None, None, None)
            .query(HistoryScope::Node, NOW, DEFAULT_HISTORY_POINTS)
            .unwrap();
        assert_eq!((q.from, q.until, q.points), (NOW - HOUR, NOW, 300));
        let q = p(Some(HistoryRange::Week), None, None, Some(50))
            .query(HistoryScope::Node, NOW, DEFAULT_HISTORY_POINTS)
            .unwrap();
        assert_eq!((q.from, q.until, q.points), (NOW - 7 * 24 * HOUR, NOW, 50));
        let q = p(None, Some(NOW - 2 * HOUR), None, None)
            .query(HistoryScope::Node, NOW, DIGEST_POINTS)
            .unwrap();
        assert_eq!((q.from, q.until, q.points), (NOW - 2 * HOUR, NOW, 120));
        let q = p(None, Some(5), Some(10), None)
            .query(HistoryScope::Node, NOW, 300)
            .unwrap();
        assert_eq!((q.from, q.until), (5, 10));
        for bad in [
            p(Some(HistoryRange::Day), Some(1), None, None),
            p(Some(HistoryRange::Day), None, Some(1), None),
            p(None, None, Some(1), None),
            p(None, Some(10), Some(10), None),
            p(None, Some(0), Some(367 * 24 * HOUR), None),
            p(None, None, None, Some(0)),
            p(None, None, None, Some(1001)),
        ] {
            let e = bad.query(HistoryScope::Node, NOW, 300).unwrap_err();
            assert_eq!(e.status, StatusCode::BAD_REQUEST, "{bad:?}");
            assert_eq!(e.code, "invalid_request");
        }
    }

    #[test]
    fn history_errors_map_to_api_errors() {
        let e = history_error("n1", HistoryError::Unsupported);
        assert_eq!(
            (e.status, e.code),
            (StatusCode::CONFLICT, "cluster_upgrading")
        );
        let e = history_error("n1", HistoryError::Unreachable("timed out".into()));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::SERVICE_UNAVAILABLE, "node_unreachable")
        );
        assert_eq!(e.message, "node \"n1\" did not answer: timed out");
        let e = history_error("n1", HistoryError::Invalid("bad".into()));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::BAD_REQUEST, "invalid_request")
        );
        let e = history_error("n1", HistoryError::Io("disk /secret".into()));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::INTERNAL_SERVER_ERROR, "internal")
        );
        assert!(!e.message.contains("secret"));
    }

    #[test]
    fn digest_sums_aligned_nodes_and_sorts_out_the_rest() {
        let start = NOW - HOUR;
        let answers = vec![
            answer(
                1,
                4,
                Ok(reply(
                    Tier::Raw5s,
                    start,
                    vec![Some(10.0), None],
                    vec![top(1, 50.0), top(9, 99.0)],
                )),
            ),
            answer(
                2,
                12,
                Ok(reply(
                    Tier::Raw5s,
                    start,
                    vec![Some(30.0), Some(40.0)],
                    vec![top(2, 70.0), top(1, 20.0)],
                )),
            ),
            // 시계가 크게 어긋나 다른 층으로 답한 노드: 열에 넣지 않지만 상위 VM은 쓴다.
            answer(
                3,
                8,
                Ok(reply(
                    Tier::Min1,
                    start,
                    vec![Some(90.0)],
                    vec![top(3, 80.0)],
                )),
            ),
            answer(4, 8, Err(HistoryError::Unsupported)),
            answer(5, 8, Err(HistoryError::Unreachable("timed out".into()))),
        ];
        let vms = vec![vm(1, 1), vm(2, 2), vm(3, 7)];
        let nodes = vec![node(1, &[]), node(2, &[])];
        let r = merge_digest(
            Uuid::from_u128(1),
            &digest_query(),
            NOW,
            answers,
            &vms,
            &nodes,
            &[],
        );
        assert_eq!((r.tier, r.start, r.step_ms), (Tier::Raw5s, start, 30_000));
        // CPU는 코어 수로 가중(10×4 + 30×12) / 16 = 25, 둘째 칸은 아는 노드만
        assert_eq!(r.columns["cpu"].avg, vec![Some(25.0), Some(40.0)]);
        assert_eq!(r.columns["mem_used"].avg, vec![Some(2048.0), Some(1024.0)]);
        assert!(r.columns["cpu"].max.is_empty());
        assert_eq!(r.node_count, 2);
        let names: Vec<_> = r.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["node1", "node2"]);
        assert_eq!(r.nodes[0].avg["cpu"], Some(10.0));
        assert_eq!(r.nodes[0].cpu, vec![Some(10.0), None]);
        assert_eq!(r.nodes[1].cpus, 12);
        assert_eq!(r.unsupported, ["node4"]);
        assert_eq!(r.unreachable, ["node5"]);
        assert_eq!(r.skewed, ["node3"]);
        // vm9는 Raft에 없어 빠지고, 두 노드에 있는 vm1은 큰 쪽 하나
        let top: Vec<_> = r
            .top
            .iter()
            .map(|t| (t.name.as_str(), t.cpu, t.node.as_deref()))
            .collect();
        assert_eq!(
            top,
            [
                ("vm3", Some(80.0), None),
                ("vm2", Some(70.0), Some("node2")),
                ("vm1", Some(50.0), Some("node1")),
            ]
        );
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["skewed"], serde_json::json!(["node3"]));
        assert_eq!(json["tier"], "raw5s");
    }

    #[test]
    fn a_tie_prefers_this_node_and_nobody_answering_still_has_a_grid() {
        let start = NOW - HOUR;
        let answers = vec![
            answer(1, 4, Ok(reply(Tier::Min1, start, vec![Some(1.0)], vec![]))),
            answer(2, 4, Ok(reply(Tier::Raw5s, start, vec![Some(2.0)], vec![]))),
        ];
        let r = merge_digest(
            Uuid::from_u128(2),
            &digest_query(),
            NOW,
            answers,
            &[],
            &[],
            &[],
        );
        assert_eq!(r.tier, Tier::Raw5s);
        assert_eq!(r.skewed, ["node1"]);

        let answers = vec![answer(1, 4, Err(HistoryError::Unsupported))];
        let r = merge_digest(
            Uuid::from_u128(2),
            &digest_query(),
            NOW,
            answers,
            &[],
            &[],
            &[],
        );
        // 1시간 = 5초 칸 721개를 120점 안으로: 7칸씩
        assert_eq!(r.tier, Tier::Raw5s);
        assert_eq!(r.step_ms, 35_000);
        assert_eq!(r.start, (NOW - HOUR).div_euclid(5_000) * 5_000);
        assert!(r.columns.is_empty());
        assert_eq!(r.node_count, 0);
        assert_eq!(r.unsupported, ["node1"]);
    }

    #[test]
    fn more_than_twenty_nodes_keep_the_busiest_rows() {
        let start = NOW - HOUR;
        let answers = (1..=30)
            .map(|n| {
                answer(
                    n,
                    4,
                    Ok(reply(Tier::Raw5s, start, vec![Some(n as f32)], vec![])),
                )
            })
            .collect();
        let r = merge_digest(
            Uuid::from_u128(1),
            &digest_query(),
            NOW,
            answers,
            &[],
            &[],
            &[],
        );
        assert_eq!(r.node_count, 30);
        assert_eq!(r.nodes.len(), MAX_CLUSTER_NODE_ROWS);
        let mut cpus: Vec<_> = r.nodes.iter().map(|n| n.avg["cpu"].unwrap()).collect();
        cpus.sort_by(f32::total_cmp);
        assert_eq!(cpus.first(), Some(&11.0));
        // 줄은 이름 순
        assert!(r.nodes.windows(2).all(|w| w[0].name < w[1].name));
    }

    #[test]
    fn pool_usage_counts_shared_pools_once_and_sums_local_ones() {
        let shared: StoragePool = Resource::new("nfs", dir_pool(true));
        let local: StoragePool = Resource::new("zfs", dir_pool(false));
        let gone: StoragePool = Resource::new("gone", dir_pool(false));
        let gib = 1u64 << 30;
        let nodes = vec![
            node(
                2,
                &[
                    ("nfs", shared.id, true, 100 * gib, 40 * gib),
                    ("zfs", local.id, true, 10 * gib, 4 * gib),
                ],
            ),
            node(
                1,
                &[
                    ("nfs", shared.id, true, 100 * gib, 30 * gib),
                    ("zfs", local.id, true, 20 * gib, 10 * gib),
                    // 같은 이름으로 다시 만들기 전의 상태는 믿지 않는다.
                    ("gone", Uuid::nil(), true, 5 * gib, 0),
                ],
            ),
        ];
        let usage = pool_usage(&[shared.clone(), local.clone(), gone], &nodes);
        assert_eq!(
            usage,
            vec![
                ClusterPoolUsage {
                    pool: shared.id,
                    name: "nfs".into(),
                    shared: true,
                    // 이름 순 첫 노드(node1)
                    used_bytes: 70 * gib,
                    total_bytes: 100 * gib,
                },
                ClusterPoolUsage {
                    pool: local.id,
                    name: "zfs".into(),
                    shared: false,
                    used_bytes: 16 * gib,
                    total_bytes: 30 * gib,
                },
            ]
        );
    }

    #[test]
    fn pool_history_asks_this_node_first_then_by_name() {
        let pool: StoragePool = Resource::new("nfs", dir_pool(true));
        let gib = 1u64 << 30;
        let nodes = vec![
            node(3, &[("nfs", pool.id, true, gib, 0)]),
            node(1, &[("nfs", pool.id, true, gib, 0)]),
            node(2, &[("nfs", pool.id, false, gib, 0)]),
        ];
        let order = |me| -> Vec<Uuid> {
            ready_nodes(&pool, &nodes, Uuid::from_u128(me))
                .iter()
                .map(|n| n.id)
                .collect()
        };
        assert_eq!(order(3), [Uuid::from_u128(3), Uuid::from_u128(1)]);
        assert_eq!(order(2), [Uuid::from_u128(1), Uuid::from_u128(3)]);
    }

    #[test]
    fn the_digest_cache_keeps_ten_seconds_and_a_bounded_number_of_keys() {
        let response = Arc::new(merge_digest(
            Uuid::nil(),
            &digest_query(),
            NOW,
            vec![],
            &[],
            &[],
            &[],
        ));
        let mut cache = DigestCache::default();
        let t0 = Instant::now();
        let key = (Some("hour"), None, None, 120);
        let fill = |cache: &mut DigestCache, key, at| {
            cache.slot(key, at).set((at, response.clone())).unwrap();
        };
        fill(&mut cache, key, t0);
        let same = |a: &DigestSlot, b: &DigestSlot| Arc::ptr_eq(a, b);
        let first = cache.slot(key, t0);
        assert!(same(&first, &cache.slot(key, t0 + Duration::from_secs(9))));
        let renewed = cache.slot(key, t0 + DIGEST_CACHE_TTL);
        assert!(!same(&first, &renewed) && renewed.get().is_none());
        // 채우는 중인 칸은 오래되어도 그대로 준다.
        assert!(same(&renewed, &cache.slot(key, t0 + DIGEST_CACHE_TTL * 5)));
        assert!(
            cache
                .slot((Some("day"), None, None, 120), t0)
                .get()
                .is_none()
        );
        for i in 0..(DIGEST_CACHE_MAX as i64 * 2) {
            fill(&mut cache, (None, Some(i), None, 120), t0);
        }
        assert_eq!(cache.entries.len(), DIGEST_CACHE_MAX);
    }

    fn empty_digest() -> ClusterHistoryResponse {
        merge_digest(Uuid::nil(), &digest_query(), NOW, vec![], &[], &[], &[])
    }

    /// 응답 없는 노드 때문에 한 조건의 모으기가 오래 걸려도 다른 조건의 요청은 기다리지 않고, 같은 조건의 요청은
    /// 모으기를 한 번만 한다(리뷰 1 #2).
    #[tokio::test(start_paused = true)]
    async fn a_slow_digest_does_not_block_other_keys_and_is_shared() {
        let cache = Arc::new(std::sync::Mutex::new(DigestCache::default()));
        let fills = Arc::new(AtomicUsize::new(0));
        let hung = (Some("hour"), None, None, 120);
        let spawn_hung = || {
            let (cache, fills) = (cache.clone(), fills.clone());
            tokio::spawn(async move {
                cached_digest(&cache, hung, || async move {
                    fills.fetch_add(1, Ordering::SeqCst);
                    // 노드 하나가 답하지 않아 기록 읽기 제한 시간까지 기다린다.
                    tokio::time::sleep(HISTORY_READ_TIMEOUT).await;
                    Ok(empty_digest())
                })
                .await
            })
        };
        let (a, b) = (spawn_hung(), spawn_hung());
        tokio::task::yield_now().await;
        let other = tokio::time::timeout(
            Duration::from_millis(10),
            cached_digest(&cache, (Some("day"), None, None, 120), || async {
                Ok(empty_digest())
            }),
        )
        .await;
        assert!(other.is_ok(), "a request for another key waited");
        assert!(a.await.unwrap().is_ok() && b.await.unwrap().is_ok());
        assert_eq!(fills.load(Ordering::SeqCst), 1);
    }

    /// 노드 300대: 한꺼번에 묻는 수는 `FAN_OUT`개까지이고, 답의 크기는 노드 수와 함께 늘지 않는다(줄 20개).
    #[tokio::test(start_paused = true)]
    async fn three_hundred_nodes_are_asked_32_at_a_time_and_the_answer_stays_small() {
        let running = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let start = NOW - HOUR;
        let answers = crate::fan_out::gather(
            (1..=300u128)
                .map(|n| {
                    let (running, peak) = (&running, &peak);
                    async move {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                        let cpu = (0..120).map(|i| Some((n + i) as f32 % 100.0)).collect();
                        let tops = (0..10).map(|i| top(n * 10 + i, (n + i) as f32)).collect();
                        answer(n, 8, Ok(reply(Tier::Raw5s, start, cpu, tops)))
                    }
                })
                .collect(),
        )
        .await;
        assert_eq!(peak.load(Ordering::SeqCst), crate::fan_out::FAN_OUT);
        let vms: Vec<Vm> = (1..=300u128)
            .flat_map(|n| (0..10).map(move |i| vm(n * 10 + i, n)))
            .collect();
        let r = merge_digest(
            Uuid::from_u128(1),
            &digest_query(),
            NOW,
            answers,
            &vms,
            &[],
            &[],
        );
        assert_eq!(r.node_count, 300);
        assert_eq!(r.nodes.len(), MAX_CLUSTER_NODE_ROWS);
        assert_eq!(r.top.len(), usize::from(DIGEST_TOP));
        let size = serde_json::to_vec(&r).unwrap().len();
        assert!(size < 64 * 1024, "{size} bytes");
    }
}
