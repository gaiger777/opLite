//! Ceph 자동 구성(계획 56)의 API `/ceph`: 부트스트랩, mon·mgr·OSD 더하기, 풀 만들기와 등록, 클라이언트 꾸러미
//! 설치(모두 admin, Task를 돌려준다), Ceph 상태 읽기와 노드의 디스크 목록. 키는 Raft에만 있고 응답에는 넣지
//! 않는다(`CephView`). Ceph 상태는 Raft가 아닌 mon 노드에서 그때그때 읽고 잠깐 둔다([`CephStatusCache`]). 다른
//! 노드의 디스크·Ceph 상태는 노드 사이 RPC(`NodeDisks`·`CephStatus`)로 읽고, 답하는 쪽은 [`CephPeerSource`]다.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use opl_cluster::types::{NodeId, Rejection};
use opl_cluster::{Cluster, ClusterError};
use opl_driver::ceph::{CephHost, CephPaths};
use opl_types::ceph::{
    CEPH_NAME, CLIENT_ID, CephCluster, CephInitRequest, CephKeys, CephNodeRequest, CephOsdRequest,
    CephPending, CephPhase, CephPoolRequest, CephReport, CephSpec, CephStatus, CephStatusView,
    CephView, DiskState, MAX_MONS, NodeDisk, default_public_network, encode_key, mon_addr,
};
use opl_types::node::{CEPH_SETUP, Node, NodeSpec};
use opl_types::resource::{Kind, Resource, ResourceSpec};
use opl_types::security_group::Cidr;
use opl_types::storage::{PoolBackend, StoragePoolSpec, pool_listed_on};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use serde::Deserialize;
use utoipa::IntoParams;
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::backups::check_online;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{check_name, check_pool_spec, lookup};
use crate::state::AppState;
use crate::tasks::create_task;

/// 다른 노드의 디스크 목록·Ceph 상태를 기다리는 시간(상태는 명령 여덟을 함께, 각 15초까지 돌린다)
const PEER_TIMEOUT: Duration = Duration::from_secs(20);

/// 자리(`CephStatus.pending`)를 잡은 Task가 아직 저장소에 없을 때 도는 것으로 보는 시간(API는 자리를 잡은 뒤 Task를
/// 만든다). 그 사이에 API가 멈췄으면 이 시간 뒤 다른 요청이 자리를 넘겨받는다.
const PENDING_GRACE: Duration = Duration::from_secs(60);

/// Ceph 상태를 마지막으로 읽어 본 결과. 웹이 10초마다 묻고 여러 화면이 함께 부르므로 잠깐 둔다(읽지 못한 것도 —
/// 닿지 않는 mon 노드를 요청마다 기다리지 않게). 읽는 동안 잠가 함께 온 요청이 한 번만 읽게 한다.
#[derive(Default)]
pub(crate) struct CephStatusCache(tokio::sync::Mutex<StatusCacheState>);

#[derive(Default)]
pub(crate) struct StatusCacheState {
    /// 마지막으로 읽어 본 때
    tried_at: Option<Instant>,
    /// 마지막으로 읽은 보고서
    report: Option<CephReport>,
    /// 마지막으로 읽어 본 것이 실패였으면 그 까닭
    error: Option<ReadError>,
}

/// `Ceph` 리소스(없으면 None)
fn find_ceph(cluster: &Cluster) -> Result<Option<CephCluster>, ApiError> {
    Ok(cluster.get_by_name::<CephSpec>(CEPH_NAME)?)
}

/// `Ceph` 리소스. 없으면 404다.
fn require_ceph(cluster: &Cluster) -> Result<CephCluster, ApiError> {
    find_ceph(cluster)?.ok_or_else(|| ApiError::not_found("no Ceph cluster is set up"))
}

/// 부트스트랩이 끝났어야 한다(409 `ceph_not_ready`).
fn require_ready(ceph: &CephCluster) -> Result<(), ApiError> {
    if ceph.status.phase != CephPhase::Ready {
        return Err(not_ready("the Ceph cluster is not ready yet"));
    }
    Ok(())
}

fn not_ready(message: impl Into<String>) -> ApiError {
    ApiError::conflict("ceph_not_ready", message)
}

fn unreachable(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "ceph_unreachable", message)
}

/// Ceph 상태를 어느 mon 노드에서도 읽지 못했다(503 `ceph_unreachable`). 막 더한 mon이 다른 mon에 닿지 못하면 mon
/// 둘의 정족수를 잃으므로 그 힌트를 붙인다.
fn status_unreachable(error: &str) -> ApiError {
    unreachable(format!(
        "cannot read the Ceph status from any monitor node: {error} (if a monitor was just added, \
         check that ports 3300 and 6789 are open between the monitor nodes — a monitor that \
         cannot join can cost the quorum)"
    ))
}

/// 요청 본문의 노드(이름이나 id). 없으면 400이다.
fn request_node(cluster: &Cluster, key: &str) -> Result<Node, ApiError> {
    lookup::<NodeSpec>(cluster, key).map_err(|e| {
        if e.status == StatusCode::NOT_FOUND {
            ApiError::bad_request(format!("node {key:?} not found"))
        } else {
            e
        }
    })
}

/// Ceph 데몬을 돌릴 노드: 있고(400), 위트니스가 아니고(400), 온라인이다(409 `node_offline`).
fn daemon_node(state: &AppState, key: &str) -> Result<Node, ApiError> {
    let node = request_node(&state.cluster, key)?;
    if node.spec.witness {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness and cannot run Ceph daemons",
            node.name
        )));
    }
    check_online(state, node.id)?;
    Ok(node)
}

/// 노드의 public 망 안 주소가 있어야 한다(400).
fn check_mon_addr(node: &Node, net: &Cidr) -> Result<(), ApiError> {
    if mon_addr(node, net).is_none() {
        return Err(ApiError::bad_request(format!(
            "node {:?} has no address in the Ceph public network {net}",
            node.name
        )));
    }
    Ok(())
}

fn parse_cidr(field: &str, s: &str) -> Result<Cidr, ApiError> {
    s.parse()
        .map_err(|e| ApiError::bad_request(format!("invalid {field} {s:?}: {e}")))
}

/// Ceph 리소스 `ceph`의 끝나지 않은 Task
fn unfinished(cluster: &Cluster, ceph: Uuid) -> Result<Vec<Task>, ApiError> {
    Ok(cluster
        .list::<TaskSpec>()?
        .1
        .into_iter()
        .filter(|t| t.spec.op.is_ceph() && t.spec.target == ceph && !t.status.state.is_finished())
        .collect())
}

/// 다른 Ceph Task `task`(노드 `node`)가 끝나지 않았다(409 `ceph_busy`). 그 노드가 꺼져 있거나 없으면 그 Task는
/// 노드가 돌아오거나 빠질 때까지 끝나지 않으므로 그것을 알린다.
fn busy(cluster: &Cluster, task: Uuid, node: Uuid) -> Result<ApiError, ApiError> {
    let mut message = format!("another Ceph task is running (task {task})");
    match cluster.get::<NodeSpec>(node)? {
        Some(n) if n.status.online => {}
        found => {
            let name = found.map_or_else(|| node.to_string(), |n| n.name);
            message.push_str(&format!(
                "; node {name:?} is offline; bring it back or remove it with `cluster remove-node`"
            ));
        }
    }
    Ok(ApiError::conflict("ceph_busy", message))
}

/// mon·mgr Task(부트스트랩 포함)는 클러스터 전체에서 한 번에 하나다(409 `ceph_busy`): 잡힌 자리(`pending`)의 Task가
/// 끝나지 않았거나(아직 만들어지지 않았으면 [`PENDING_GRACE`] 동안), 끝나지 않은 그런 Task가 있으면 거절한다.
/// `abandoned`(꺼지거나 지워진 옛 부트스트랩 노드)의 Task는 버려진 것으로 본다.
fn check_busy(
    cluster: &Cluster,
    ceph: &CephCluster,
    abandoned: Option<Uuid>,
) -> Result<(), ApiError> {
    if let Some(p) = &ceph.status.pending
        && Some(p.node) != abandoned
        && pending_running(cluster, p)?
    {
        return Err(busy(cluster, p.task, p.node)?);
    }
    let tasks = unfinished(cluster, ceph.id)?;
    let running = tasks.iter().find(|t| {
        matches!(
            t.spec.op,
            TaskOp::CephInit | TaskOp::CephMonAdd | TaskOp::CephMgrAdd
        ) && Some(t.spec.node) != abandoned
    });
    match running {
        Some(t) => Err(busy(cluster, t.id, t.spec.node)?),
        None => Ok(()),
    }
}

/// 자리를 잡은 Task가 아직 끝나지 않았는지(아직 만들어지지 않았으면 [`PENDING_GRACE`] 동안은 도는 것으로 본다)
fn pending_running(cluster: &Cluster, p: &CephPending) -> Result<bool, ApiError> {
    Ok(match cluster.get::<TaskSpec>(p.task)? {
        Some(t) => !t.status.state.is_finished(),
        None => made_within(p.task, PENDING_GRACE),
    })
}

/// UUIDv7 `id`가 지금부터 `within` 안에 만들어졌는지
fn made_within(id: Uuid, within: Duration) -> bool {
    let Some(ts) = id.get_timestamp() else {
        return false;
    };
    let (secs, nanos) = ts.to_unix();
    let made = UNIX_EPOCH + Duration::new(secs, nanos);
    SystemTime::now()
        .duration_since(made)
        .map_or(true, |age| age < within)
}

/// 한 번에 하나인 Ceph Task(`op`, 노드 `node`)를 만든다: 새로 읽은 리소스로 `check`(바쁜지 포함)를 하고, 상태를
/// `change`로 바꾸며 자리(`pending`)를 리비전 확인 쓰기로 잡은 뒤(그사이 다른 노드의 API가 썼으면 다시 읽어
/// 확인한다) Task를 만든다. Task를 만들지 못하면 자리를 놓는다.
async fn create_exclusive(
    state: &AppState,
    op: TaskOp,
    node: Uuid,
    check: impl Fn(&CephCluster) -> Result<(), ApiError>,
    change: impl Fn(&mut CephStatus),
) -> Result<Task, ApiError> {
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: op.clone(),
            target: Uuid::nil(),
            node,
        },
    );
    task.name = task.id.to_string();
    loop {
        let ceph = require_ceph(&state.cluster)?;
        check(&ceph)?;
        let mut status = ceph.status.clone();
        change(&mut status);
        status.pending = Some(CephPending {
            op: op.clone(),
            node,
            task: task.id,
        });
        match state
            .cluster
            .update_status_checked::<CephSpec>(ceph.id, ceph.revision, &status)
            .await
        {
            Ok(_) => {
                task.spec.target = ceph.id;
                break;
            }
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    match state.cluster.create(&task).await {
        Ok(revision) => {
            task.revision = revision;
            Ok(task)
        }
        Err(e) => {
            release_pending(&state.cluster, task.id).await;
            Err(e.into())
        }
    }
}

/// Task `task`가 잡은 자리(`pending`)를 놓는다(다른 Task의 자리면 그대로). 실패는 로그만 남긴다 — 끝난 Task의 자리는
/// 어차피 넘겨받는다.
pub(crate) async fn release_pending(cluster: &Cluster, task: Uuid) {
    loop {
        let ceph = match cluster.get_by_name::<CephSpec>(CEPH_NAME) {
            Ok(Some(ceph)) if ceph.status.pending.as_ref().is_some_and(|p| p.task == task) => ceph,
            Ok(_) => return,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the Ceph cluster");
                return;
            }
        };
        let mut status = ceph.status;
        status.pending = None;
        match cluster
            .update_status_checked::<CephSpec>(ceph.id, ceph.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "cannot release the Ceph task slot");
                return;
            }
            Ok(_) => return,
        }
    }
}

/// 새 Ceph 비밀 키(`ceph-authtool --gen-print-key` 모양)
fn new_key() -> String {
    let mut secret = [0u8; 16];
    getrandom::fill(&mut secret).expect("system random source");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    encode_key(&secret, now.as_secs() as u32, now.subsec_nanos())
}

/// 노드가 관리 Ceph에 붙는 클라이언트 conf(`[ceph] client_dir`)
fn managed_conf(state: &AppState) -> String {
    let path = match &state.ceph {
        Some(host) => host.client_conf(),
        None => CephPaths::default().client_dir.join("ceph.conf"),
    };
    path.to_string_lossy().into_owned()
}

/// 노드 `node`가 쓰는 Orbivisor LVM 풀(LVM-thin·공유 LVM)의 (VG, 풀 이름). 그 PV는 OSD로 쓰지 않는다.
pub(crate) fn pool_vgs(cluster: &Cluster, node: Uuid) -> Result<Vec<(String, String)>, String> {
    let (_, pools) = cluster
        .list::<StoragePoolSpec>()
        .map_err(|e| e.to_string())?;
    Ok(pools
        .into_iter()
        .filter(|p| pool_listed_on(p, node))
        .filter_map(|p| match &p.spec.backend {
            PoolBackend::LvmThin { vg, .. } | PoolBackend::LvmShared { vg, .. } => {
                Some((vg.clone(), p.name.clone()))
            }
            _ => None,
        })
        .collect())
}

/// 이 노드(`node`)의 디스크 목록
async fn local_disks(
    cluster: &Cluster,
    node: Uuid,
    host: &CephHost,
) -> Result<Vec<NodeDisk>, String> {
    let vgs = pool_vgs(cluster, node)?;
    let fsid = cluster
        .get_by_name::<CephSpec>(CEPH_NAME)
        .map_err(|e| e.to_string())?
        .map(|c| c.spec.fsid);
    host.disks(&vgs, fsid).await.map_err(|e| e.to_string())
}

/// 이 노드(`node`, mon 노드)에서 읽은 Ceph 상태. 관리 conf를 쓰는 RBD 풀을 Ceph 풀에 잇는다.
async fn local_report(
    cluster: &Cluster,
    node: Uuid,
    host: &CephHost,
) -> Result<CephReport, String> {
    let conf = host.client_conf().to_string_lossy().into_owned();
    let (_, pools) = cluster
        .list::<StoragePoolSpec>()
        .map_err(|e| e.to_string())?;
    let pools: Vec<(String, String)> = pools
        .into_iter()
        .filter_map(|p| match p.spec.backend {
            PoolBackend::Rbd {
                pool,
                conf: Some(c),
                ..
            } if c == conf => Some((p.name, pool)),
            _ => None,
        })
        .collect();
    let mut report = host.report(&pools).await.map_err(|e| e.to_string())?;
    report.read_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    report.via = node;
    Ok(report)
}

/// 다른 노드가 노드 사이 RPC로 묻는 이 노드의 디스크 목록과 Ceph 상태. opld가 노드 간 서버에 넣는다(위트니스
/// 제외).
pub struct CephPeerSource {
    cluster: Arc<Cluster>,
    node: Uuid,
    host: Arc<CephHost>,
}

impl CephPeerSource {
    /// `host`는 이 노드의 Task 실행기·API와 같은 것이다.
    pub fn new(cluster: Arc<Cluster>, node: Uuid, host: Arc<CephHost>) -> Self {
        Self {
            cluster,
            node,
            host,
        }
    }
}

#[async_trait]
impl opl_peer::CephSource for CephPeerSource {
    async fn disks(&self) -> Result<Vec<NodeDisk>, String> {
        local_disks(&self.cluster, self.node, &self.host).await
    }

    async fn status(&self) -> Result<CephReport, String> {
        local_report(&self.cluster, self.node, &self.host).await
    }
}

/// 노드 `node`의 디스크 목록: 이 노드면 직접, 아니면 노드 사이 RPC로. 위트니스·Ceph 단계가 없는 노드면 409
/// `node_unavailable`, 오프라인이면 409 `node_offline`, 이 RPC를 모르는 옛 노드면 409 `cluster_upgrading`, 읽지
/// 못하면 503 `node_unreachable`.
async fn read_disks(state: &AppState, node: &Node) -> Result<Vec<NodeDisk>, ApiError> {
    let failed = |why: &str| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("cannot read the disks of node {:?}: {why}", node.name),
        )
    };
    if node.spec.witness {
        return Err(ApiError::conflict(
            "node_unavailable",
            format!("node {:?} is a witness and has no Ceph disks", node.name),
        ));
    }
    if node.id == state.node {
        let host = state.ceph.as_ref().ok_or_else(|| {
            ApiError::conflict(
                "node_unavailable",
                format!("node {:?} cannot run Ceph", node.name),
            )
        })?;
        return local_disks(&state.cluster, node.id, host)
            .await
            .map_err(|e| failed(&e));
    }
    check_online(state, node.id)?;
    let members = state.cluster.members();
    let addr = members
        .addr(NodeId(node.id))
        .ok_or_else(|| failed("not a cluster member"))?;
    let peers = state
        .peers
        .as_ref()
        .ok_or_else(|| failed("no node-to-node connection"))?;
    let read = peers.node_disks(NodeId(node.id), addr, PEER_TIMEOUT);
    match tokio::time::timeout(PEER_TIMEOUT, read).await {
        Ok(Ok(Some(disks))) => Ok(disks),
        Ok(Ok(None)) => Err(ApiError::conflict(
            "cluster_upgrading",
            format!(
                "node {:?} does not support ceph setup yet; upgrade it first",
                node.name
            ),
        )),
        Ok(Err(status)) => Err(failed(status.message())),
        Err(_) => Err(failed("timed out")),
    }
}

/// Ceph 상태를 읽지 못한 까닭
#[derive(Clone)]
enum ReadError {
    /// 물은 mon 노드가 모두 이 RPC를 모른다(옛 노드).
    Upgrading(String),
    Failed(String),
}

/// mon 노드에서 Ceph 상태를 읽는다: 이 노드가 mon이면 직접, 아니면 온라인 mon 노드에 id 순으로 노드 사이 RPC로
/// 물어 첫 답.
async fn read_report(state: &AppState, ceph: &CephCluster) -> Result<CephReport, ReadError> {
    let mons = &ceph.status.mons;
    if mons.is_empty() {
        return Err(ReadError::Failed(
            "the Ceph cluster has no monitor yet".into(),
        ));
    }
    if let Some(host) = &state.ceph
        && mons.iter().any(|m| m.node == state.node)
    {
        return local_report(&state.cluster, state.node, host)
            .await
            .map_err(ReadError::Failed);
    }
    let mut nodes: Vec<Node> = Vec::new();
    for mon in mons {
        if let Some(node) = state
            .cluster
            .get::<NodeSpec>(mon.node)
            .map_err(|e| ReadError::Failed(e.to_string()))?
            && node.status.online
        {
            nodes.push(node);
        }
    }
    nodes.sort_by_key(|n| n.id);
    let mut last = "no monitor node is online".to_string();
    let mut upgrading = 0;
    let members = state.cluster.members();
    // 모든 mon 노드에 함께 묻고 먼저 온 답을 쓴다(닿지 않는 노드를 차례로 기다리지 않게).
    let mut reads: FuturesUnordered<_> = nodes
        .iter()
        .map(|node| {
            let addr = members.addr(NodeId(node.id)).map(str::to_string);
            async move {
                let (Some(addr), Some(peers)) = (addr, state.peers.as_ref()) else {
                    return (node, None);
                };
                let read = peers.ceph_status(NodeId(node.id), &addr, PEER_TIMEOUT);
                (node, Some(tokio::time::timeout(PEER_TIMEOUT, read).await))
            }
        })
        .collect();
    while let Some((node, result)) = reads.next().await {
        match result {
            None => last = format!("node {:?} is not reachable", node.name),
            Some(Ok(Ok(Some(report)))) => return Ok(report),
            Some(Ok(Ok(None))) => {
                upgrading += 1;
                last = format!(
                    "node {:?} does not support ceph setup yet; upgrade it first",
                    node.name
                );
            }
            Some(Ok(Err(status))) => last = format!("node {:?}: {}", node.name, status.message()),
            Some(Err(_)) => last = format!("node {:?}: timed out", node.name),
        }
    }
    if upgrading > 0 && upgrading == nodes.len() {
        return Err(ReadError::Upgrading(last));
    }
    Err(ReadError::Failed(last))
}

/// Ceph 상태(캐시를 거침): `refresh`가 아니고 마지막으로 읽어 본 지 `ceph_status_ttl`이 지나지 않았으면 그 결과를,
/// 아니면 새로 읽는다. 마지막 결과가 실패면 마지막 보고서를 `stale`로, 그것도 없으면 503 `ceph_unreachable`(옛
/// 노드뿐이면 409 `cluster_upgrading`)이다.
async fn status_view(
    state: &AppState,
    ceph: &CephCluster,
    refresh: bool,
) -> Result<CephStatusView, ApiError> {
    let mut cache = state.ceph_status.0.lock().await;
    let recent = cache
        .tried_at
        .is_some_and(|at| at.elapsed() < state.ceph_status_ttl);
    if refresh || !recent {
        let result = read_report(state, ceph).await;
        cache.tried_at = Some(Instant::now());
        match result {
            Ok(report) => {
                cache.report = Some(report);
                cache.error = None;
            }
            Err(e) => cache.error = Some(e),
        }
    }
    let error = match (&cache.error, &cache.report) {
        (None, Some(report)) => {
            return Ok(CephStatusView {
                report: report.clone(),
                stale: false,
                error: None,
            });
        }
        (Some(ReadError::Upgrading(e)), None) => {
            return Err(ApiError::conflict("cluster_upgrading", e.clone()));
        }
        (Some(ReadError::Upgrading(e) | ReadError::Failed(e)), _) => e.clone(),
        (None, None) => "the Ceph status was not read yet".to_string(),
    };
    match &cache.report {
        Some(report) => Ok(CephStatusView {
            report: report.clone(),
            stale: true,
            error: Some(error),
        }),
        None => Err(status_unreachable(&error)),
    }
}

/// Orbivisor가 관리하는 Ceph 클러스터(키는 넣지 않는다). 끝나지 않은 Ceph Task의 id를 함께 보인다. 없으면 404다.
#[utoipa::path(get, path = "/ceph", tag = "ceph",
    responses((status = 200, body = CephView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_ceph(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<CephView>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    let tasks = unfinished(&state.cluster, ceph.id)?
        .into_iter()
        .map(|t| t.id)
        .collect();
    Ok(Json(CephView::new(&ceph, managed_conf(&state), tasks)))
}

/// Ceph를 세운다: 키·fsid를 만들고 `Ceph` 리소스(`bootstrapping`)와 노드 `node`의 `ceph-init` Task(202)를 만든다.
/// 그 노드가 Ceph 꾸러미를 설치하고 첫 mon·mgr을 세운다. `public_network`가 없으면 노드 주소가 든 인터페이스의
/// 망이다. 부트스트랩이 실패했으면 같은 노드로 다시 부르면 새 Task만 만들어 이어서 한다. 노드가 없거나 위트니스·망
/// 문법·망 안 주소가 없으면 400, 오프라인이면 409 `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`,
/// 이미 있으면(다른 노드에서 세우는 중 포함) 409 `ceph_exists`, 부트스트랩 Task가 끝나지 않았으면 409 `ceph_busy`.
#[utoipa::path(post, path = "/ceph", tag = "ceph",
    request_body = CephInitRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn init_ceph(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephInitRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    let node = daemon_node(&state, &req.node)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    if let Some(ceph) = find_ceph(&state.cluster)? {
        return retry_init(&state, &audit, &req, &node, &ceph).await;
    }
    let public_network = match &req.public_network {
        Some(s) => parse_cidr("public_network", s)?,
        None => default_public_network(&node).ok_or_else(|| {
            ApiError::bad_request(format!(
                "give public_network: cannot tell the network of node {:?} from its address {:?}",
                node.name, node.spec.address
            ))
        })?,
    };
    let cluster_network = req
        .cluster_network
        .as_deref()
        .map(|s| parse_cidr("cluster_network", s))
        .transpose()?;
    check_mon_addr(&node, &public_network)?;
    let mut ceph: CephCluster = Resource::new(
        CEPH_NAME,
        CephSpec {
            fsid: Uuid::new_v4(),
            public_network,
            cluster_network,
            keys: CephKeys {
                mon: new_key(),
                admin: new_key(),
                bootstrap_osd: new_key(),
                client: new_key(),
            },
        },
    );
    // 부트스트랩 Task의 자리도 함께 잡는다(리소스 만들기는 한 요청만 성공한다).
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: TaskOp::CephInit,
            target: ceph.id,
            node: node.id,
        },
    );
    task.name = task.id.to_string();
    ceph.status = CephStatus {
        init_node: Some(node.id),
        pending: Some(CephPending {
            op: TaskOp::CephInit,
            node: node.id,
            task: task.id,
        }),
        ..CephStatus::default()
    };
    match state.cluster.create(&ceph).await {
        Ok(_) => {}
        Err(opl_cluster::ClusterError::Rejected(Rejection::NameTaken { .. })) => {
            return Err(ApiError::conflict(
                "ceph_exists",
                "a Ceph cluster is already set up",
            ));
        }
        Err(e) => return Err(e.into()),
    }
    audit.detail(format!(
        "ceph init on {} (public {public_network})",
        node.name
    ));
    match state.cluster.create(&task).await {
        Ok(revision) => task.revision = revision,
        Err(e) => {
            release_pending(&state.cluster, task.id).await;
            return Err(e.into());
        }
    }
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 이미 있는 `Ceph` 리소스에 `POST /ceph`: 같은 노드에서 세우는 중(실패 뒤)이면 새 `ceph-init` Task만 만든다. 부트스트랩
/// 노드가 지워졌거나 꺼져 있고 아직 mon이 하나도 서지 않았으면 이 노드로 옮겨 다시 한다(같은 fsid·키).
async fn retry_init(
    state: &AppState,
    audit: &AuditNote,
    req: &CephInitRequest,
    node: &Node,
    ceph: &CephCluster,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    // 망은 만든 뒤 바뀌지 않는다.
    let given = [
        (
            "public_network",
            &req.public_network,
            Some(ceph.spec.public_network),
        ),
        (
            "cluster_network",
            &req.cluster_network,
            ceph.spec.cluster_network,
        ),
    ];
    for (field, given, have) in given {
        if let Some(s) = given
            && Some(parse_cidr(field, s)?) != have
        {
            return Err(ApiError::conflict(
                "ceph_exists",
                format!(
                    "the Ceph cluster being set up uses {field} {}; retry without {field}",
                    have.map_or_else(|| "none".to_string(), |c| c.to_string())
                ),
            ));
        }
    }
    let check = |ceph: &CephCluster| -> Result<(), ApiError> {
        // 준비된 클러스터는 mon을 모두 잃었어도 다시 세우지 않는다(남은 OSD의 데이터가 그 mon에 묶여 있다).
        if ceph.status.phase == CephPhase::Ready && ceph.status.mons.is_empty() {
            return Err(ApiError::conflict(
                "ceph_exists",
                format!(
                    "the Ceph cluster (fsid {}) is set up but Orbivisor knows none of its monitors; \
                     Orbivisor does not set it up again — recover a monitor by hand, or remove the \
                     Ceph record from the cluster state (Raft) by hand before a new ceph init",
                    ceph.spec.fsid
                ),
            ));
        }
        if ceph.status.phase == CephPhase::Ready {
            return Err(ApiError::conflict(
                "ceph_exists",
                format!("a Ceph cluster is already set up (fsid {})", ceph.spec.fsid),
            ));
        }
        let abandoned = moved_init(state, ceph, node)?;
        check_busy(&state.cluster, ceph, abandoned)
    };
    check(ceph)?;
    let moved = match moved_init(state, ceph, node)? {
        Some(old) => format!(
            ", moved from {}",
            crate::backups::node_name(state, old).unwrap_or_else(|_| old.to_string())
        ),
        None => String::new(),
    };
    audit.detail(format!(
        "ceph init on {} (public {}, retry{moved})",
        node.name, ceph.spec.public_network
    ));
    let old = moved_init(state, ceph, node)?;
    let task = create_exclusive(state, TaskOp::CephInit, node.id, check, |s| {
        s.init_node = Some(node.id)
    })
    .await?;
    if let Some(old) = old {
        abandon_tasks(&state.cluster, ceph.id, old, &node.name).await?;
    }
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 부트스트랩을 노드 `old`에서 `to`로 옮겼으면 `old`의 끝나지 않은 Ceph Task를 실패로 끝낸다(리비전 확인 쓰기 — 옛
/// 노드가 돌아와도 실행기가 `pending`이 아닌 Task를 시작하지 않는다). 이미 돌고 있던 Task는 부트스트랩 Task가
/// 스스로 멈춘다.
async fn abandon_tasks(cluster: &Cluster, ceph: Uuid, old: Uuid, to: &str) -> Result<(), ApiError> {
    let failed = TaskStatus {
        state: opl_types::task::TaskState::Failed,
        error: Some(format!("the Ceph bootstrap was moved to node {to:?}")),
        progress: None,
    };
    loop {
        let tasks: Vec<Task> = unfinished(cluster, ceph)?
            .into_iter()
            .filter(|t| t.spec.node == old)
            .collect();
        let Some(t) = tasks.first() else {
            return Ok(());
        };
        match cluster
            .update_status_checked::<TaskSpec>(t.id, t.revision, &failed)
            .await
        {
            Ok(_) | Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
            Err(ClusterError::Rejected(Rejection::NotFound { .. })) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// 부트스트랩을 다른 노드 `node`에서 다시 하려 하면: 옛 부트스트랩 노드가 지워졌거나 꺼져 있고 아직 mon이 없으면 그
/// 노드(의 버려진 Task)를 돌려주고 옮기게 한다. 아니면 409 `ceph_exists`(그 노드에서 다시 하라).
fn moved_init(state: &AppState, ceph: &CephCluster, node: &Node) -> Result<Option<Uuid>, ApiError> {
    let Some(init) = ceph.status.init_node.filter(|init| *init != node.id) else {
        return Ok(None);
    };
    let other = state.cluster.get::<NodeSpec>(init)?;
    if ceph.status.mons.is_empty() && other.as_ref().is_none_or(|n| !n.status.online) {
        return Ok(Some(init));
    }
    let name = other.map_or_else(|| init.to_string(), |n| n.name);
    Err(ApiError::conflict(
        "ceph_exists",
        format!("Ceph is being set up on node {name:?}; retry there"),
    ))
}

/// 노드 `node`에 mon을 더하는 `ceph-mon-add` Task(202). mon은 5개까지(409 `ceph_limit`)이고 홀수(3·5)가 정족수를
/// 가장 잘 지킨다. 부트스트랩 전이면 409 `ceph_not_ready`, 다른 mon·mgr Task가 끝나지 않았으면 409 `ceph_busy`,
/// 그 노드에 이미 mon이 있으면 409 `ceph_exists`, 노드가 없거나 위트니스·망 안 주소가 없으면 400.
#[utoipa::path(post, path = "/ceph/mons", tag = "ceph",
    request_body = CephNodeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn add_mon(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephNodeRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    let node = daemon_node(&state, &req.node)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    let check = |ceph: &CephCluster| -> Result<(), ApiError> {
        require_ready(ceph)?;
        if ceph.status.mons.iter().any(|m| m.node == node.id) {
            return Err(ApiError::conflict(
                "ceph_exists",
                format!("node {:?} already runs a monitor", node.name),
            ));
        }
        // 더하는 중인 mon도 센다.
        let adding = match &ceph.status.pending {
            Some(p)
                if p.op == TaskOp::CephMonAdd
                    && !ceph.status.mons.iter().any(|m| m.node == p.node)
                    && pending_running(&state.cluster, p)? =>
            {
                1
            }
            _ => 0,
        };
        if ceph.status.mons.len() + adding >= MAX_MONS {
            return Err(ApiError::conflict(
                "ceph_limit",
                format!("a Ceph cluster here can have at most {MAX_MONS} monitors"),
            ));
        }
        check_busy(&state.cluster, ceph, None)?;
        check_mon_addr(&node, &ceph.spec.public_network)
    };
    check(&ceph)?;
    check_monmap(&state, &ceph, &node).await?;
    audit.detail(format!("ceph mon add {}", node.name));
    let task = create_exclusive(&state, TaskOp::CephMonAdd, node.id, check, |_| {}).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// mon을 더하기 전에 Ceph의 monmap이 Raft와 맞는지 본다. Orbivisor가 모르는 mon(앞서 실패한 더하기가 남긴 것 등 —
/// 더하려는 이 노드의 것은 다시 하기라 괜찮다)이나 정족수 밖의 mon이 있으면 새 mon이 정족수를 잃게 할 수 있으므로
/// 409 `ceph_busy`로 거절하고 손으로 치우는 명령을 알린다. 상태를 지금 읽지 못하면 503 `ceph_unreachable`.
async fn check_monmap(state: &AppState, ceph: &CephCluster, node: &Node) -> Result<(), ApiError> {
    let view = status_view(state, ceph, true).await?;
    if view.stale {
        return Err(status_unreachable(&view.error.unwrap_or_default()));
    }
    for mon in view.report.mons.iter().filter(|m| m.name != node.name) {
        let why = if !ceph.status.mons.iter().any(|m| m.name == mon.name) {
            "is in the Ceph monitor map but not in Orbivisor"
        } else if !mon.in_quorum {
            "is out of quorum"
        } else {
            continue;
        };
        return Err(ApiError::conflict(
            "ceph_busy",
            format!(
                "monitor {} {why}; bring it back or remove it by hand with: ceph mon remove {}",
                mon.name, mon.name
            ),
        ));
    }
    Ok(())
}

/// 노드 `node`에 (대기) mgr을 더하는 `ceph-mgr-add` Task(202). 오류는 mon 더하기와 같다(상한은 없다 — 노드마다
/// 하나).
#[utoipa::path(post, path = "/ceph/mgrs", tag = "ceph",
    request_body = CephNodeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn add_mgr(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephNodeRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    let node = daemon_node(&state, &req.node)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    let check = |ceph: &CephCluster| -> Result<(), ApiError> {
        require_ready(ceph)?;
        check_busy(&state.cluster, ceph, None)?;
        if ceph.status.mgrs.contains(&node.id) {
            return Err(ApiError::conflict(
                "ceph_exists",
                format!("node {:?} already runs a manager", node.name),
            ));
        }
        Ok(())
    };
    check(&ceph)?;
    audit.detail(format!("ceph mgr add {}", node.name));
    let task = create_exclusive(&state, TaskOp::CephMgrAdd, node.id, check, |_| {}).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 노드 `node`의 디스크 `device`(`/dev/sdb`나 by-id 경로)에 OSD를 만드는 `ceph-osd-add` Task(202). 먼저 그 노드의
/// 디스크 목록으로 확인한다: 그런 디스크가 없으면 400, 쓰는 중이면 409 `disk_in_use`, 비지 않았는데 `wipe`가
/// 아니면 409 `disk_not_empty`. Task에는 by-id 경로가 있으면 그것을 적는다(재부팅으로 이름이 바뀌어도 같은 디스크).
/// 부트스트랩 전이면 409 `ceph_not_ready`.
#[utoipa::path(post, path = "/ceph/osds", tag = "ceph",
    request_body = CephOsdRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn add_osd(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephOsdRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    let node = daemon_node(&state, &req.node)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    require_ready(&ceph)?;
    let disks = read_disks(&state, &node).await?;
    let disk = disks
        .into_iter()
        .find(|d| d.path == req.device || d.by_id.as_deref() == Some(req.device.as_str()))
        .ok_or_else(|| {
            ApiError::bad_request(format!("node {:?} has no disk {:?}", node.name, req.device))
        })?;
    match &disk.state {
        DiskState::Available => {}
        DiskState::InUse { reason } => {
            return Err(ApiError::conflict(
                "disk_in_use",
                format!("{} cannot hold an OSD: {reason}", disk.path),
            ));
        }
        DiskState::Wipeable { reasons } if !req.wipe => {
            return Err(ApiError::conflict(
                "disk_not_empty",
                format!(
                    "{} is not empty ({}); pass wipe to erase it",
                    disk.path,
                    reasons.join(", ")
                ),
            ));
        }
        DiskState::Wipeable { .. } => {}
    }
    audit.detail(format!(
        "ceph osd add {} {}{}",
        node.name,
        disk.path,
        if req.wipe { " (wipe)" } else { "" }
    ));
    let device = disk.by_id.unwrap_or(disk.path);
    let op = TaskOp::CephOsdAdd {
        device,
        wipe: req.wipe,
    };
    let task = create_task(&state.cluster, op, ceph.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// Ceph 풀을 만들어 Orbivisor RBD 풀 `name`(사용자 `orbivisor`, 관리 conf)으로 등록하는 `ceph-pool-create` Task(202,
/// 온라인 mon 노드 가운데 id가 가장 작은 노드에서 돈다). `pool`이 없으면 Ceph 풀 이름은 `name`, `size`가 없으면
/// min(3, OSD가 up인 호스트 수)다(호스트가 하나면 size 1을 적어야 한다). size는 1~3이고 호스트 수보다 클 수
/// 없다(400). 이름이 잘못됐으면 400, 이미 있는 풀 이름이면 409 `pool_exists`, 부트스트랩 전이거나 OSD가 없으면
/// 409 `ceph_not_ready`, Ceph 상태를 읽지 못하면 503 `ceph_unreachable`.
#[utoipa::path(post, path = "/ceph/pools", tag = "ceph",
    request_body = CephPoolRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn create_ceph_pool(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephPoolRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    state.authorize(&principal, Action::Write, StoragePoolSpec::KIND)?;
    let ceph = require_ceph(&state.cluster)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    require_ready(&ceph)?;
    check_name(&req.name)?;
    let pool = req.pool.clone().unwrap_or_else(|| req.name.clone());
    let mut spec = StoragePoolSpec {
        backend: PoolBackend::Rbd {
            pool: pool.clone(),
            namespace: None,
            user: Some(CLIENT_ID.to_string()),
            conf: Some(managed_conf(&state)),
        },
        nodes: Vec::new(),
        shared: true,
        backups: false,
    };
    check_pool_spec(&state.cluster, &mut spec, true)?;
    // 이미 등록된 Ceph 풀(어느 conf로든)은 다시 만들지 않는다 — 이어서 하기가 쓰는 풀의 복제 수를 바꾸지 않게.
    let (_, pools) = state.cluster.list::<StoragePoolSpec>()?;
    if let Some(other) = pools
        .iter()
        .find(|p| matches!(&p.spec.backend, PoolBackend::Rbd { pool: used, .. } if *used == pool))
    {
        return Err(ApiError::conflict(
            "pool_exists",
            format!(
                "storage pool {:?} already uses the Ceph pool {pool}; choose another Ceph pool name",
                other.name
            ),
        ));
    }
    if state
        .cluster
        .get_by_name::<StoragePoolSpec>(&req.name)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "pool_exists",
            Rejection::NameTaken {
                kind: StoragePoolSpec::KIND,
                name: req.name.clone(),
            }
            .to_string(),
        ));
    }
    if let Some(size) = req.size
        && !(1..=3).contains(&size)
    {
        return Err(ApiError::bad_request("size must be 1, 2 or 3"));
    }
    let view = status_view(&state, &ceph, false).await?;
    if view.stale {
        return Err(status_unreachable(&view.error.unwrap_or_default()));
    }
    let hosts = view.report.osd_hosts_up;
    let size = match (req.size, hosts) {
        (_, 0) => return Err(not_ready("add OSDs first")),
        (None, 1) => {
            return Err(ApiError::bad_request(
                "only one host has OSDs; pass size 1 to keep a single copy (no redundancy)",
            ));
        }
        (None, h) => h.min(3) as u8,
        (Some(s), h) if u32::from(s) > h => {
            return Err(ApiError::bad_request(format!(
                "size {s} needs OSDs on at least {s} hosts (now {h})"
            )));
        }
        (Some(s), _) => s,
    };
    let node = pool_node(&state, &ceph)?;
    audit.detail(format!("ceph pool create {} size {size}", req.name));
    let op = TaskOp::CephPoolCreate {
        name: req.name,
        pool,
        size,
    };
    let task = create_task(&state.cluster, op, ceph.id, node).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 클러스터에서 뺀 노드 `node`를 Orbivisor의 Ceph 기록(`mons`·`mgrs`·`osd_nodes`)에서 뺀다. Ceph 쪽(monmap·OSD)은
/// 바꾸지 않으므로 손으로 치울 명령을 경고로 돌려준다(Ceph 데몬 노드가 아니었으면 없음).
pub(crate) async fn forget_node(state: &AppState, node: &Node) -> Result<Vec<String>, ApiError> {
    loop {
        let Some(ceph) = find_ceph(&state.cluster)? else {
            return Ok(Vec::new());
        };
        let mut status = ceph.status.clone();
        status.mons.retain(|m| m.node != node.id);
        status.mgrs.retain(|n| *n != node.id);
        status.osd_nodes.retain(|n| *n != node.id);
        if status == ceph.status {
            return Ok(Vec::new());
        }
        match state
            .cluster
            .update_status_checked::<CephSpec>(ceph.id, ceph.revision, &status)
            .await
        {
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(e.into()),
            Ok(_) => {}
        }
        let mut warnings = Vec::new();
        if let Some(mon) = ceph.status.mons.iter().find(|m| m.node == node.id) {
            warnings.push(format!(
                "node {:?} ran a Ceph monitor; remove it from the Ceph monitor map by hand with: \
                 ceph mon remove {}",
                node.name, mon.name
            ));
        }
        if ceph.status.osd_nodes.contains(&node.id) {
            warnings.push(format!(
                "node {:?} had Ceph OSDs; they stay in Ceph as down until you remove them by hand \
                 (ceph osd purge ID --yes-i-really-mean-it)",
                node.name
            ));
        }
        if ceph.status.mgrs.contains(&node.id) {
            warnings.push(format!(
                "node {:?} ran a Ceph manager; remove its key by hand with: ceph auth del mgr.{}",
                node.name, node.name
            ));
        }
        tracing::info!(node = %node.name, "removed a deleted node from the Ceph cluster record");
        return Ok(warnings);
    }
}

/// 풀 Task를 돌릴 노드: 온라인 mon 노드 가운데 id가 가장 작은 것(없으면 409 `node_offline`)
fn pool_node(state: &AppState, ceph: &CephCluster) -> Result<Uuid, ApiError> {
    let mut online = Vec::new();
    for mon in &ceph.status.mons {
        if state
            .cluster
            .get::<NodeSpec>(mon.node)?
            .is_some_and(|n| n.status.online)
        {
            online.push(mon.node);
        }
    }
    online
        .into_iter()
        .min()
        .ok_or_else(|| ApiError::conflict("node_offline", "no monitor node is online"))
}

/// 노드 `node`에 Ceph 클라이언트 꾸러미(`ceph-common`·`qemu-block-extra`)를 설치하는 `ceph-client-install`
/// Task(202). 클라이언트 설정은 노드마다 이미 쓰여 있다. 노드가 없거나 위트니스면 400, 오프라인이면 409
/// `node_offline`.
#[utoipa::path(post, path = "/ceph/clients", tag = "ceph",
    request_body = CephNodeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn install_client(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CephNodeRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    let node = request_node(&state.cluster, &req.node)?;
    if node.spec.witness {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness and runs no VMs",
            node.name
        )));
    }
    check_online(&state, node.id)?;
    require_feature(&state.cluster, CEPH_SETUP)?;
    audit.detail(format!("ceph client install {}", node.name));
    let task = create_task(&state.cluster, TaskOp::CephClientInstall, ceph.id, node.id).await?;
    Ok((StatusCode::ACCEPTED, Json(task)))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct CephStatusQuery {
    /// 잠깐 둔 상태를 쓰지 않고 새로 읽는다.
    #[serde(default)]
    pub refresh: bool,
}

/// Ceph 상태(health·mon·mgr·OSD·PG·사용량·풀). Raft가 아닌 mon 노드에서 읽는다(이 노드가 mon이면 직접, 아니면 온라인
/// mon 노드에 노드 사이 RPC로). 잠깐(`[ceph] status_cache_secs`, 기본 5초) 둔 것을 돌려주고 `refresh`면 새로 읽는다.
/// 지금 읽지 못하면 마지막 보고서를 `stale`과 `error`로 돌려주고, 그것도 없으면 503 `ceph_unreachable`이다. mon
/// 노드가 이 RPC를 모르면 409 `cluster_upgrading`, 없으면 404.
#[utoipa::path(get, path = "/ceph/status", tag = "ceph",
    params(CephStatusQuery),
    responses((status = 200, body = CephStatusView), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_ceph_status(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(q): Query<CephStatusQuery>,
) -> Result<Json<CephStatusView>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::Ceph)?;
    let ceph = require_ceph(&state.cluster)?;
    Ok(Json(status_view(&state, &ceph, q.refresh).await?))
}

/// 노드의 디스크(`lsblk`의 최상위 디스크)마다 OSD로 쓸 수 있는지: `available`, `wipeable`(지워야 함, 이유들),
/// `in-use`(쓸 수 없음, 이유). 위트니스면 409 `node_unavailable`, 오프라인이면 409 `node_offline`, 이것을 모르는 옛
/// 노드면 409 `cluster_upgrading`, 읽지 못하면 503 `node_unreachable`.
#[utoipa::path(get, path = "/nodes/{key}/disks", tag = "ceph",
    params(("key" = String, Path, description = "node id or name")),
    responses((status = 200, body = Vec<NodeDisk>), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn list_node_disks(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<Vec<NodeDisk>>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    Ok(Json(read_disks(&state, &node).await?))
}
