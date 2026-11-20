//! SDN(OVN, 계획 65 §13)의 API: `/sdn`(첫 중앙 세우기·중앙 더하기·빼기·chassis 설치 — 모두 admin, Task를 돌려준다 —
//! 와 상태), SDN 네트워크의 서브넷 바꾸기(`PATCH /networks/{key}`)와 주소 목록(`GET /networks/{key}/ips`).
//!
//! 중앙 Task(`sdn-init`·`sdn-central-add`·`sdn-central-remove`)는 클러스터 전체에서 한 번에 하나다: API가
//! `SdnStatus.pending`을 리비전 확인 쓰기로 잡은 뒤 Task를 만든다(계획 56 Ceph와 같다 — 잡힌 자리의 Task가 끝나지
//! 않았으면 409 `sdn_busy`). chassis 설치는 노드마다 함께 돈다. API는 Raft만 쓰고 OVN을 부르지 않는다(주소 목록의 `live`만
//! NB·SB를 읽는다).

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::ipam;
use opl_types::network::{Network, NetworkSpec, NetworkStatus, Subnet};
use opl_types::node::{Node, NodeSpec, SDN, SDN_ROUTING};
use opl_types::resource::{Kind, Resource};
use opl_types::sdn::{
    MAX_CENTRALS, SDN_NAME, Sdn, SdnChassisRequest, SdnDeparted, SdnInitRequest, SdnNodeRequest,
    SdnPending, SdnPhase, SdnSpec, SdnStatus, SdnView, central_address,
};
use opl_types::task::{Task, TaskOp, TaskSpec, TaskStatus};
use opl_types::vm::VmSpec;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::backups::check_online;
use crate::ceph::{PENDING_GRACE, made_within};
use crate::cluster::{host_of, require_feature};
use crate::error::{ApiError, Json, Query};
use crate::resources::{if_match, lookup};
use crate::revision::WARNING;
use crate::state::AppState;
use crate::tasks::create_task;

/// macOS 노드의 하이퍼바이저(OVN chassis가 될 수 없다)
const HVF: &str = "qemu-hvf";

/// `Sdn` 리소스(없으면 None)
fn find_sdn(cluster: &Cluster) -> Result<Option<Sdn>, ApiError> {
    Ok(cluster.get_by_name::<SdnSpec>(SDN_NAME)?)
}

fn not_ready(message: impl Into<String>) -> ApiError {
    ApiError::conflict("sdn_not_ready", message)
}

/// SDN이 세워졌어야 한다(없거나 세우는 중이면 409 `sdn_not_ready`).
pub(crate) fn require_ready(cluster: &Cluster) -> Result<Sdn, ApiError> {
    match find_sdn(cluster)? {
        Some(sdn) if sdn.status.phase == SdnPhase::Ready && !sdn.status.centrals.is_empty() => {
            Ok(sdn)
        }
        Some(_) => Err(not_ready(
            "SDN (OVN) is still being set up; wait for `sdn init` to finish",
        )),
        None => Err(not_ready(
            "SDN (OVN) is not set up; run `orbivisorctl sdn init NODE` first",
        )),
    }
}

/// 계획 66 SDN 라우팅의 새 칸·종류(공급자 네트워크, 서브넷 `floating_pools`·`ipv6_mode`, 노드 `sdn`, 할당량
/// `routers`·`floating_ips`, 라우터·유동 IP 옮기기)는 API 스테이지(계획 66 스테이지 5) 전까지 쓰지 않는다: 옛 노드가 남았으면
/// 409 `cluster_upgrading`(검증보다 먼저 — 옛 노드는 이 칸을 풀지 못한다), 아니면 400. 스테이지 5가 지운다.
pub(crate) fn routing_not_yet(cluster: &Cluster, what: &str) -> ApiError {
    match require_feature(cluster, SDN_ROUTING) {
        Err(e) => e,
        Ok(()) => ApiError::bad_request(format!("{what} are not available yet (SDN routing)")),
    }
}

/// 서브넷이 계획 66의 칸(`floating_pools`·`ipv6_mode`)을 쓴다.
pub(crate) fn subnet_uses_routing(subnet: &Subnet) -> bool {
    !subnet.floating_pools.is_empty() || subnet.ipv6_mode.is_some()
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

/// OVN을 돌릴 노드: 위트니스·macOS가 아니고(400), Geneve 주소(`NodeStatus.underlay`)가 있고(400), 온라인이다(409
/// `node_offline`).
fn ovn_node(state: &AppState, node: Node) -> Result<Node, ApiError> {
    if node.spec.witness {
        return Err(ApiError::bad_request(format!(
            "node {:?} is a witness and cannot run OVN",
            node.name
        )));
    }
    if node.status.hypervisor == HVF {
        return Err(ApiError::bad_request(format!(
            "node {:?} runs macOS; SDN (OVN) needs Linux nodes",
            node.name
        )));
    }
    if node.status.underlay.is_none() {
        return Err(ApiError::bad_request(format!(
            "node {:?} has no underlay address (the Geneve tunnel address); set [network] underlay",
            node.name
        )));
    }
    check_online(state, node.id)?;
    Ok(node)
}

/// 중앙을 돌릴 노드: [`ovn_node`]이고 유지보수 중이 아니다(400).
fn central_node(state: &AppState, key: &str) -> Result<Node, ApiError> {
    let node = ovn_node(state, request_node(&state.cluster, key)?)?;
    if node.spec.maintenance {
        return Err(ApiError::bad_request(format!(
            "node {:?} is in maintenance; end it before making it an OVN central",
            node.name
        )));
    }
    Ok(node)
}

/// 노드 인증서의 이름(SAN): 노드 주소와 클러스터 링크의 호스트, 같은 기계의 이름(opld `cert_names`와 같다)
fn cert_names(node: &Node) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let hosts = host_of(&node.spec.address)
        .into_iter()
        .chain(
            node.status
                .cluster_addresses
                .iter()
                .filter_map(|a| host_of(a)),
        )
        .chain(["localhost", "127.0.0.1", "::1"]);
    for host in hosts {
        if !names.iter().any(|n| n == host) {
            names.push(host.to_string());
        }
    }
    names
}

/// 노드의 주소(노드 주소와 클러스터 링크의 호스트 가운데 IP인 것): 중앙 주소가 될 수 있는 것
fn node_ips(node: &Node) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = Vec::new();
    let hosts = host_of(&node.spec.address).into_iter().chain(
        node.status
            .cluster_addresses
            .iter()
            .filter_map(|a| host_of(a)),
    );
    for ip in hosts.filter_map(|h| h.parse::<IpAddr>().ok()) {
        let ip = ip.to_canonical();
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    ips
}

/// 중앙 주소 `address`(주지 않았으면 노드 주소의 IP나 underlay)를 정한다(리뷰 1 I-5). 준 주소는 노드의 주소(노드 주소와
/// 클러스터 링크의 호스트 — 노드 인증서의 이름 안이라 OvnSync가 그 이름으로 NB·SB 인증서를 확인한다)여야 하고, 지정 안 함
/// (`0.0.0.0`·`::`)·링크 로컬·멀티캐스트는 늘, 루프백은 노드 자신의 주소가 루프백일 때(한 기계의 클러스터)가 아니면
/// 거절한다(400) — 다른 중앙과 chassis가 닿을 수 없다. 정할 수 없으면 400.
fn central_ip(node: &Node, address: Option<IpAddr>) -> Result<IpAddr, ApiError> {
    let ip = match address {
        Some(ip) => ip.to_canonical(),
        None => central_address(node).ok_or_else(|| {
            ApiError::bad_request(format!(
                "cannot tell the OVN central address of node {:?} from its address {:?}; give address",
                node.name, node.spec.address
            ))
        })?,
    };
    let link_local = match ip {
        IpAddr::V4(v) => v.is_link_local(),
        IpAddr::V6(v) => v.is_unicast_link_local(),
    };
    if ip.is_unspecified() || ip.is_multicast() || link_local {
        return Err(ApiError::bad_request(format!(
            "address {ip} cannot be an OVN central address (other centrals and chassis must reach it); \
             use a unicast address of node {:?}",
            node.name
        )));
    }
    let own = node_ips(node);
    if address.is_some() && !own.contains(&ip) {
        let names = cert_names(node);
        return Err(ApiError::bad_request(format!(
            "address {ip} is not in the certificate of node {:?} ({}); use one of its addresses",
            node.name,
            names
                .iter()
                .filter(|n| n.parse::<IpAddr>().is_ok_and(|a| own.contains(&a)))
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    if ip.is_loopback() && (own.is_empty() || !own.iter().all(IpAddr::is_loopback)) {
        return Err(ApiError::bad_request(format!(
            "address {ip} is a loopback address; other centrals and chassis cannot reach it — use an address of node {:?}",
            node.name
        )));
    }
    Ok(ip)
}

/// SDN Task(`op`가 sdn-*) 가운데 끝나지 않은 것
fn unfinished(cluster: &Cluster) -> Result<Vec<Task>, ApiError> {
    Ok(cluster
        .list::<TaskSpec>()?
        .1
        .into_iter()
        .filter(|t| t.spec.op.is_sdn() && !t.status.state.is_finished())
        .collect())
}

/// 중앙 Task(첫 세우기·중앙 더하기·빼기)인지
fn is_central_op(op: &TaskOp) -> bool {
    matches!(
        op,
        TaskOp::SdnInit { .. } | TaskOp::SdnCentralAdd | TaskOp::SdnCentralRemove
    )
}

/// 다른 중앙 Task `task`(노드 `node`)가 끝나지 않았다(409 `sdn_busy`). 그 노드가 꺼져 있거나 없으면 알린다.
fn busy(cluster: &Cluster, task: Uuid, node: Uuid) -> Result<ApiError, ApiError> {
    let mut message = format!("another SDN central task is running (task {task})");
    match cluster.get::<NodeSpec>(node)? {
        Some(n) if n.status.online => {}
        found => {
            let name = found.map_or_else(|| node.to_string(), |n| n.name);
            message.push_str(&format!(
                "; node {name:?} is offline; bring it back or remove it with `cluster remove-node`"
            ));
        }
    }
    Ok(ApiError::conflict("sdn_busy", message))
}

/// 자리를 잡은 Task가 아직 끝나지 않았는지(아직 만들어지지 않았으면 [`PENDING_GRACE`] 동안은 도는 것으로 본다)
fn pending_running(cluster: &Cluster, p: &SdnPending) -> Result<bool, ApiError> {
    Ok(match cluster.get::<TaskSpec>(p.task)? {
        Some(t) => !t.status.state.is_finished(),
        None => made_within(p.task, PENDING_GRACE),
    })
}

/// 중앙 Task는 한 번에 하나다(409 `sdn_busy`): 잡힌 자리의 Task가 끝나지 않았거나 끝나지 않은 중앙 Task가 있으면
/// 거절한다. `abandoned`(꺼지거나 지워진 옛 첫 세우기 노드)의 Task는 버려진 것으로 본다.
fn check_busy(cluster: &Cluster, sdn: &Sdn, abandoned: Option<Uuid>) -> Result<(), ApiError> {
    if let Some(p) = &sdn.status.pending
        && Some(p.node) != abandoned
        && pending_running(cluster, p)?
    {
        return Err(busy(cluster, p.task, p.node)?);
    }
    let tasks = unfinished(cluster)?;
    match tasks
        .iter()
        .find(|t| is_central_op(&t.spec.op) && Some(t.spec.node) != abandoned)
    {
        Some(t) => Err(busy(cluster, t.id, t.spec.node)?),
        None => Ok(()),
    }
}

/// 한 번에 하나인 중앙 Task(`op`, 노드 `node`)를 만든다: 새로 읽은 리소스로 `check`를 하고, 상태를 `change`로 바꾸며
/// 자리(`pending`)를 리비전 확인 쓰기로 잡은 뒤 Task를 만든다. 리소스가 없으면(첫 세우기) 자리를 가진 리소스를 만든다.
/// Task를 만들지 못하면 자리를 놓는다.
async fn create_exclusive(
    state: &AppState,
    op: TaskOp,
    node: Uuid,
    check: impl Fn(Option<&Sdn>) -> Result<(), ApiError>,
    change: impl Fn(&mut SdnStatus),
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
    if let Some(user) = crate::audit::requester() {
        task.labels
            .insert(crate::tasks::REQUESTED_BY.to_string(), user.to_string());
    }
    let pending = SdnPending {
        op: op.clone(),
        node,
        task: task.id,
    };
    loop {
        let found = find_sdn(&state.cluster)?;
        check(found.as_ref())?;
        let Some(sdn) = found else {
            let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec {});
            change(&mut sdn.status);
            sdn.status.pending = Some(pending.clone());
            match state.cluster.create(&sdn).await {
                Ok(_) => {
                    task.spec.target = sdn.id;
                    break;
                }
                // 다른 요청이 그새 만들었다: 다시 읽어 확인한다.
                Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => continue,
                Err(e) => return Err(e.into()),
            }
        };
        let mut status = sdn.status.clone();
        change(&mut status);
        status.pending = Some(pending.clone());
        match state
            .cluster
            .update_status_checked::<SdnSpec>(sdn.id, sdn.revision, &status)
            .await
        {
            Ok(_) => {
                task.spec.target = sdn.id;
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

/// Task `task`가 잡은 자리를 놓는다(다른 Task의 자리면 그대로). 실패는 로그만 남긴다.
async fn release_pending(cluster: &Cluster, task: Uuid) {
    let result = crate::tasks::sdn::update_sdn(cluster, |s| {
        if s.pending.as_ref().is_some_and(|p| p.task == task) {
            s.pending = None;
            return true;
        }
        false
    })
    .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "cannot release the SDN task slot");
    }
}

/// SDN(OVN)의 상태: 단계, 중앙과 각 중앙이 알린 Raft 역할·northd, 리더 OvnSync의 마지막 동기화, 위트니스가 아닌 노드마다의
/// chassis 상태, 끝나지 않은 SDN Task. 설정 전이면 `phase`가 없다.
#[utoipa::path(get, path = "/sdn", tag = "sdn",
    responses((status = 200, body = SdnView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_sdn(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<SdnView>, ApiError> {
    state.authorize(&principal, Action::Read, Kind::Sdn)?;
    let sdn = find_sdn(&state.cluster)?;
    let (_, nodes) = state.cluster.list::<NodeSpec>()?;
    let tasks = unfinished(&state.cluster)?
        .into_iter()
        .map(|t| t.id)
        .collect();
    Ok(Json(SdnView::new(sdn.as_ref(), &nodes, tasks)))
}

/// SDN(OVN)을 세운다: 노드 `node`에 OVN 꾸러미를 apt로 설치하고 NB·SB ovsdb Raft 클러스터와 northd를 Orbivisor 단위로
/// 세우고 chassis로 구성하는 `sdn-init` Task(202). `address`는 NB·SB가 들을 주소(없으면 노드 주소의 IP, 노드 인증서의
/// 이름 안의 노드 주소여야 하고 루프백·링크 로컬은 안 된다). 실패했으면 같은 노드로 다시 부르면 이어서 하고(주소를
/// 주지 않으면 지난번 주소로), 첫 세우기 노드가 꺼졌거나 지워졌고 아직 중앙이 없으면 다른 노드로 옮겨 다시 한다. 노드가 없거나 위트니스·macOS·유지보수·underlay 없음·인증서 밖 주소면 400,
/// 오프라인이면 409 `node_offline`, 옛 노드가 남았으면 409 `cluster_upgrading`, 이미 세웠거나 다른 노드에서 세우는 중이면
/// 409 `sdn_exists`, 다른 중앙 Task가 끝나지 않았으면 409 `sdn_busy`.
#[utoipa::path(post, path = "/sdn", tag = "sdn",
    request_body = SdnInitRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn init_sdn(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SdnInitRequest>,
) -> Result<(StatusCode, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Sdn)?;
    require_feature(&state.cluster, SDN)?;
    let node = central_node(&state, &req.node)?;
    let existing = find_sdn(&state.cluster)?;
    // 같은 노드에서 끝나지 않은 첫 세우기를 주소 없이 다시 하면 지난번 주소로 이어서 한다(리뷰 1 I-7 — 기본 주소로
    // 바뀌면 이미 만든 DB의 Raft 주소와 단위가 어긋난다).
    let given = req.address.or_else(|| {
        existing
            .as_ref()
            .filter(|s| s.status.phase != SdnPhase::Ready && s.status.init_node == Some(node.id))
            .and_then(|s| s.status.init_address)
    });
    let address = central_ip(&node, given)?;
    let moved = match &existing {
        Some(sdn) => moved_init(&state, sdn, &node)?,
        None => None,
    };
    let check = |sdn: Option<&Sdn>| -> Result<(), ApiError> {
        let Some(sdn) = sdn else {
            return Ok(());
        };
        let s = &sdn.status;
        if s.phase == SdnPhase::Ready {
            // 준비된 SDN에서는 첫 세우기 노드가 아직 중앙일 때만 다시 한다(멈춘 단위·빠진 chassis 고치기).
            if s.init_node != Some(node.id) || !s.centrals.iter().any(|c| c.node == node.id) {
                return Err(ApiError::conflict(
                    "sdn_exists",
                    "SDN (OVN) is already set up; add central nodes with `sdn central add`",
                ));
            }
            if let Some(c) = s.centrals.iter().find(|c| c.node == node.id)
                && req.address.is_some_and(|a| a.to_canonical() != c.address)
            {
                return Err(ApiError::conflict(
                    "sdn_exists",
                    format!(
                        "node {:?} is an OVN central on {}; retry without address",
                        node.name, c.address
                    ),
                ));
            }
        }
        let abandoned = moved_init(&state, sdn, &node)?;
        check_busy(&state.cluster, sdn, abandoned)
    };
    let mut detail = format!("sdn init {} (address {address})", node.name);
    if existing.is_some() {
        detail.push_str(", retry");
    }
    if let Some(old) = moved {
        let name = crate::backups::node_name(&state, old).unwrap_or_else(|_| old.to_string());
        detail.push_str(&format!(", moved from {name}"));
    }
    audit.detail(detail);
    let op = TaskOp::SdnInit {
        address: given.map(|a| a.to_canonical()),
    };
    let task = create_exclusive(&state, op, node.id, check, |s| {
        s.init_node = Some(node.id);
        if s.phase != SdnPhase::Ready {
            s.init_address = Some(address);
        }
    })
    .await?;
    if let Some(old) = moved {
        abandon_tasks(&state.cluster, old, &node.name).await?;
    }
    Ok((StatusCode::ACCEPTED, Json(task)))
}

/// 첫 세우기를 다른 노드 `node`에서 다시 하려 하면: 옛 첫 세우기 노드가 지워졌거나 꺼져 있고 아직 중앙이 없으면 그
/// 노드를 돌려주고 옮기게 한다. 아니면 409 `sdn_exists`(그 노드에서 다시 하라). 같은 노드면 None
fn moved_init(state: &AppState, sdn: &Sdn, node: &Node) -> Result<Option<Uuid>, ApiError> {
    let Some(init) = sdn.status.init_node.filter(|init| *init != node.id) else {
        return Ok(None);
    };
    if sdn.status.phase == SdnPhase::Ready {
        // 준비된 SDN의 다시 하기는 위에서 거절한다.
        return Ok(None);
    }
    let other = state.cluster.get::<NodeSpec>(init)?;
    if sdn.status.centrals.is_empty() && other.as_ref().is_none_or(|n| !n.status.online) {
        return Ok(Some(init));
    }
    let name = other.map_or_else(|| init.to_string(), |n| n.name);
    Err(ApiError::conflict(
        "sdn_exists",
        format!("SDN (OVN) is being set up on node {name:?}; retry there"),
    ))
}

/// 첫 세우기를 노드 `old`에서 `to`로 옮겼으면 `old`의 끝나지 않은 SDN Task를 실패로 끝낸다(리비전 확인 쓰기). 이미 돌고
/// 있던 Task는 자리를 잃어 스스로 멈춘다.
async fn abandon_tasks(cluster: &Cluster, old: Uuid, to: &str) -> Result<(), ApiError> {
    let failed = TaskStatus {
        state: opl_types::task::TaskState::Failed,
        error: Some(format!("the SDN (OVN) setup was moved to node {to:?}")),
        progress: None,
    };
    loop {
        let tasks: Vec<Task> = unfinished(cluster)?
            .into_iter()
            .filter(|t| t.spec.node == old && is_central_op(&t.spec.op))
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

/// 중앙 수가 짝수면 그 경고(장애를 하나 적은 홀수만큼만 견딘다)
fn even_warning(count: usize) -> Option<String> {
    count.is_multiple_of(2).then(|| {
        format!(
            "SDN (OVN) will have {count} central nodes; an even number tolerates no more failures than one fewer; \
             use 3 or 5"
        )
    })
}

/// 응답 헤더 `orbivisor-warning`(있을 때만)
fn warning_header(warning: Option<String>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(value) = warning.and_then(|w| HeaderValue::from_str(&w).ok()) {
        headers.insert(WARNING, value);
    }
    headers
}

/// 노드 `node`를 OVN 중앙으로 더하는 `sdn-central-add` Task(202): NB·SB Raft 클러스터에 합류하고 northd를 띄우고
/// chassis로 구성한다. 중앙은 5개까지(409 `sdn_limit`)이고 홀수(3·5)가 정족수를 가장 잘 지킨다(짝수가 되면 응답 헤더
/// `orbivisor-warning`). SDN이 준비되지 않았으면 409 `sdn_not_ready`, 이미 중앙이면 409 `sdn_exists`, 다른 중앙 Task가
/// 끝나지 않았으면 409 `sdn_busy`, 노드 오류는 `POST /sdn`과 같다.
#[utoipa::path(post, path = "/sdn/centrals", tag = "sdn",
    request_body = SdnNodeRequest,
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>,
        headers(("orbivisor-warning" = String, description = "the central count becomes even (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn add_central(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SdnNodeRequest>,
) -> Result<(StatusCode, HeaderMap, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Sdn)?;
    require_feature(&state.cluster, SDN)?;
    require_ready(&state.cluster)?;
    let node = central_node(&state, &req.node)?;
    central_ip(&node, None)?;
    let check = |sdn: Option<&Sdn>| -> Result<(), ApiError> {
        let Some(sdn) = sdn else {
            return Err(not_ready("SDN (OVN) is not set up"));
        };
        if sdn.status.phase != SdnPhase::Ready {
            return Err(not_ready("SDN (OVN) is still being set up"));
        }
        if sdn.status.centrals.iter().any(|c| c.node == node.id) {
            return Err(ApiError::conflict(
                "sdn_exists",
                format!("node {:?} is already an OVN central", node.name),
            ));
        }
        if sdn.status.centrals.len() >= MAX_CENTRALS {
            return Err(ApiError::conflict(
                "sdn_limit",
                format!("SDN (OVN) can have at most {MAX_CENTRALS} central nodes"),
            ));
        }
        check_busy(&state.cluster, sdn, None)
    };
    let count = require_ready(&state.cluster)?.status.centrals.len() + 1;
    audit.detail(format!("sdn central add {}", node.name));
    let task = create_exclusive(&state, TaskOp::SdnCentralAdd, node.id, check, |_| {}).await?;
    Ok((
        StatusCode::ACCEPTED,
        warning_header(even_warning(count)),
        Json(task),
    ))
}

/// 노드 `node`를 OVN 중앙에서 빼는 Task(202). 노드가 온라인이면 그 노드의 `sdn-central-remove`: NB·SB Raft 클러스터를
/// 떠나고 단위를 멈추고 DB 파일은 이름만 바꾼다(지우지 않음 — chassis 구성은 그대로). 꺼졌거나 클러스터에서 지운
/// 노드면 살아 있는 다른 중앙의 `sdn-central-kick`: 그 노드의 Raft 주소를 NB·SB 클러스터에서 내쫓고 목록에서 뺀다(리뷰 2
/// N-1 — 그 노드의 단위와 DB는 손대지 못한다). 중앙 목록에 없지만 이 SDN의 DB를 가진 노드(끝나지 않은 중앙 더하기가 남긴
/// 멤버)와 지운 노드 기록(`strays`)도 같이 받는다(마지막 중앙 검사·짝수 경고 없음). 마지막 중앙은 뺄 수 없다(409
/// `sdn_last_central`). 노드가 중앙이 아니면 400(없으면 404), 내쫓을 살아 있는 중앙이 없으면 409 `node_offline`, SDN이
/// 준비되지 않았으면 409 `sdn_not_ready`, 다른 중앙 Task가 끝나지 않았으면 409 `sdn_busy`. 목록의 중앙을 빼서 짝수가 되면
/// 응답 헤더 `orbivisor-warning`.
#[utoipa::path(delete, path = "/sdn/centrals/{node}", tag = "sdn",
    params(("node" = String, Path, description = "node id or name (a deleted node: the name or id `strays` shows)")),
    responses((status = 202, body = Resource<TaskSpec, TaskStatus>,
        headers(("orbivisor-warning" = String, description = "the central count becomes even (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn remove_central(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
) -> Result<(StatusCode, HeaderMap, Json<Task>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Sdn)?;
    require_feature(&state.cluster, SDN)?;
    let sdn = require_ready(&state.cluster)?;
    let target = removal_target(&state.cluster, &sdn, &key)?;
    let id = target.id;
    let alive = target.node.as_ref().is_some_and(|n| n.status.online);
    let check = |sdn: Option<&Sdn>| -> Result<(), ApiError> {
        let Some(sdn) = sdn.filter(|s| s.status.phase == SdnPhase::Ready) else {
            return Err(not_ready("SDN (OVN) is not set up"));
        };
        let listed = sdn.status.centrals.iter().any(|c| c.node == id);
        let departed = sdn.status.departed.iter().any(|d| d.node == id);
        if !listed && !departed && !target.node.as_ref().is_some_and(half_member) {
            return Err(ApiError::bad_request(format!(
                "node {:?} is not an OVN central",
                target.name
            )));
        }
        if listed && sdn.status.centrals.len() <= 1 {
            return Err(ApiError::conflict(
                "sdn_last_central",
                "the last OVN central node cannot be removed",
            ));
        }
        check_busy(&state.cluster, sdn, None)
    };
    check(Some(&sdn))?;
    let centrals = &sdn.status.centrals;
    let listed = centrals.iter().any(|c| c.node == id);
    // 목록의 중앙을 뺄 때만 수가 바뀐다(리뷰 2 m-4 — 목록 밖의 멤버를 치울 때는 경고하지 않는다).
    let warning = listed.then(|| even_warning(centrals.len() - 1)).flatten();
    let task = if alive {
        audit.detail(format!("sdn central remove {}", target.name));
        create_exclusive(&state, TaskOp::SdnCentralRemove, id, check, |_| {}).await?
    } else {
        let address = target.address.ok_or_else(|| {
            ApiError::bad_request(format!(
                "cannot tell the OVN central address of node {:?}; kick it out by hand on a central with \
                 `ovs-appctl -t /run/opl/ovn/nb.ctl cluster/kick OVN_Northbound SERVER` (and sb.ctl OVN_Southbound)",
                target.name
            ))
        })?;
        let by = live_central(&state.cluster, &sdn, id)?.ok_or_else(|| {
            ApiError::conflict(
                "node_offline",
                format!(
                    "node {:?} is offline and no other OVN central is online to kick it out of the OVN cluster",
                    target.name
                ),
            )
        })?;
        audit.detail(format!(
            "sdn central remove {} (offline; kicked out by {})",
            target.name, by.name
        ));
        create_exclusive(
            &state,
            TaskOp::SdnCentralKick { node: id, address },
            by.id,
            check,
            |_| {},
        )
        .await?
    };
    Ok((StatusCode::ACCEPTED, warning_header(warning), Json(task)))
}

/// 중앙 빼기의 대상: 노드(있으면)와 그 Raft 주소(목록의 주소, 지운 노드 기록의 주소, 아니면 노드의 기본 중앙 주소)
struct RemovalTarget {
    id: Uuid,
    name: String,
    node: Option<Node>,
    address: Option<IpAddr>,
}

/// `DELETE /sdn/centrals/{key}`의 `key`: 노드 이름·id, 없으면 지운 노드 기록(`departed`)의 이름·id나 목록에 남은 중앙의
/// id(옛 `cluster leave`가 남긴 것 — 리뷰 2 N-1). 어느 것도 아니면 404.
fn removal_target(cluster: &Cluster, sdn: &Sdn, key: &str) -> Result<RemovalTarget, ApiError> {
    let listed = |id: Uuid| {
        sdn.status
            .centrals
            .iter()
            .find(|c| c.node == id)
            .map(|c| c.address)
    };
    match lookup::<NodeSpec>(cluster, key) {
        Ok(node) => Ok(RemovalTarget {
            id: node.id,
            name: node.name.clone(),
            address: listed(node.id).or_else(|| central_address(&node)),
            node: Some(node),
        }),
        Err(e) if e.status == StatusCode::NOT_FOUND => {
            if let Some(d) = sdn
                .status
                .departed
                .iter()
                .find(|d| d.name == key || d.node.to_string() == key)
            {
                return Ok(RemovalTarget {
                    id: d.node,
                    name: d.name.clone(),
                    node: None,
                    address: Some(d.address),
                });
            }
            match sdn
                .status
                .centrals
                .iter()
                .find(|c| c.node.to_string() == key)
            {
                Some(c) => Ok(RemovalTarget {
                    id: c.node,
                    name: c.node.to_string(),
                    node: None,
                    address: Some(c.address),
                }),
                None => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

/// 노드 `except` 말고 온라인인 중앙(Raft에서 다른 서버를 내쫓을 노드). 없으면 None
fn live_central(cluster: &Cluster, sdn: &Sdn, except: Uuid) -> Result<Option<Node>, ApiError> {
    for c in sdn.status.centrals.iter().filter(|c| c.node != except) {
        if let Some(n) = cluster.get::<NodeSpec>(c.node)?
            && n.status.online
        {
            return Ok(Some(n));
        }
    }
    Ok(None)
}

/// 노드들(`nodes`, 또는 `all`이면 아직 준비된 chassis가 아닌 모든 온라인 Linux 노드)에 OVN chassis 꾸러미를 깔고 구성하는
/// `sdn-chassis-install` Task(노드마다 하나, 함께 돈다, 202). 새로 합류한 노드에 저절로 깔지 않는다. `all`이 건너뛴
/// 오프라인 노드는 응답 헤더 `orbivisor-warning`으로 알린다. `nodes`와 `all`을 함께·둘 다 안 주면 400, SDN이 준비되지
/// 않았으면 409 `sdn_not_ready`, 노드 오류는 `POST /sdn`과 같다(유지보수 노드도 된다).
#[utoipa::path(post, path = "/sdn/chassis", tag = "sdn",
    request_body = SdnChassisRequest,
    responses((status = 202, body = [Resource<TaskSpec, TaskStatus>],
        headers(("orbivisor-warning" = String, description = "offline nodes `all` skipped (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn install_chassis(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SdnChassisRequest>,
) -> Result<(StatusCode, HeaderMap, Json<Vec<Task>>), ApiError> {
    state.authorize(&principal, Action::Write, Kind::Sdn)?;
    req.validate().map_err(ApiError::bad_request)?;
    require_feature(&state.cluster, SDN)?;
    require_ready(&state.cluster)?;
    let mut skipped = Vec::new();
    let nodes: Vec<Node> = if req.all {
        let (_, mut all) = state.cluster.list::<NodeSpec>()?;
        all.sort_by(|a, b| a.name.cmp(&b.name));
        let mut picked = Vec::new();
        for n in all {
            let ready = n
                .status
                .sdn
                .as_ref()
                .is_some_and(|s| s.chassis == opl_types::sdn::ChassisState::Ready);
            if n.spec.witness || n.status.hypervisor == HVF || ready {
                continue;
            }
            if !n.status.online {
                skipped.push(n.name.clone());
                continue;
            }
            picked.push(ovn_node(&state, n)?);
        }
        if picked.is_empty() {
            return Err(ApiError::bad_request(
                "every online Linux node is already a ready SDN (OVN) chassis",
            ));
        }
        picked
    } else {
        let mut picked: Vec<Node> = Vec::new();
        for key in &req.nodes {
            let n = ovn_node(&state, request_node(&state.cluster, key)?)?;
            if !picked.iter().any(|p| p.id == n.id) {
                picked.push(n);
            }
        }
        picked
    };
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    audit.detail(format!("sdn chassis install {}", names.join(",")));
    let mut tasks = Vec::new();
    for node in &nodes {
        tasks.push(create_task(&state.cluster, TaskOp::SdnChassisInstall, node.id, node.id).await?);
    }
    let warning = (!skipped.is_empty()).then(|| {
        format!(
            "skipped offline nodes: {}; install them when they are back",
            skipped.join(", ")
        )
    });
    Ok((StatusCode::ACCEPTED, warning_header(warning), Json(tasks)))
}

/// 중앙 목록에 없지만 이 SDN의 NB·SB DB를 가진 노드(실패한 `sdn-central-add`가 합류만 하고 남긴 멤버 — 리뷰 1 I-7).
/// 노드의 `SdnAgent`가 알린다(`NodeSdn.central`, 준비된 SDN에서만).
fn half_member(node: &Node) -> bool {
    node.status
        .sdn
        .as_ref()
        .is_some_and(|s| s.central.is_some())
}

/// 꺼진 노드 `node`를 지우기 전에 본다(설계 §7.1): 목록의 OVN 중앙이면 409 `sdn_central_on_node`(먼저 `sdn central
/// remove` — 꺼진 노드는 다른 중앙이 내쫓는다). 목록 밖에서 이 SDN의 DB를 알린 노드(반쯤 합류한 멤버)는 막지 않고 지운 뒤
/// 내쫓을 기록을 돌려준다(리뷰 2 N-2 — 죽은 노드를 영영 지우지 못하게 되지 않게). 부르는 쪽이 노드를 지운 뒤
/// [`kick_departed`]로 넘긴다.
pub(crate) fn check_node_removal(
    cluster: &Cluster,
    node: &Node,
) -> Result<Option<SdnDeparted>, ApiError> {
    let Some(sdn) = find_sdn(cluster)? else {
        return Ok(None);
    };
    if let Some(c) = sdn.status.centrals.iter().find(|c| c.node == node.id) {
        return Err(ApiError::conflict(
            "sdn_central_on_node",
            format!(
                "node {:?} is an OVN central ({}); remove it with `orbivisorctl sdn central remove {}` first \
                 (another central kicks an offline node out of the OVN cluster)",
                node.name, c.address, node.name
            ),
        ));
    }
    if sdn.status.phase != SdnPhase::Ready || !half_member(node) {
        return Ok(None);
    }
    Ok(Some(SdnDeparted {
        node: node.id,
        name: node.name.clone(),
        address: central_address(node).unwrap_or(IpAddr::from([0, 0, 0, 0])),
    }))
}

/// 지운 노드 `departed`가 OVN Raft에 남았을 수 있다: 기록(`SdnStatus.departed`)에 적고 살아 있는 중앙의
/// `sdn-central-kick` Task로 내쫓는다(리뷰 2 N-2). 할 수 없으면(살아 있는 중앙이 없음, 다른 중앙 Task가 돎) 기록만 두고
/// 나중에 `sdn central remove NAME`을 하라고 알린다. 노드 지우기 응답의 경고 줄을 돌려준다.
pub(crate) async fn kick_departed(state: &AppState, departed: SdnDeparted) -> Vec<String> {
    let name = departed.name.clone();
    if departed.address.is_unspecified() {
        return vec![format!(
            "node {name:?} held OVN central databases but its address is unknown; on a central run \
             `ovs-appctl -t /run/opl/ovn/nb.ctl cluster/status OVN_Northbound` and kick its server with \
             `cluster/kick` (and the same for sb.ctl OVN_Southbound)"
        )];
    }
    let record = departed.clone();
    if let Err(e) = crate::tasks::sdn::update_sdn(&state.cluster, |s| {
        if s.departed.iter().any(|d| d.node == record.node) {
            return false;
        }
        s.departed.push(record.clone());
        true
    })
    .await
    {
        return vec![format!(
            "node {name:?} held OVN central databases and could not be recorded ({e}); kick its server \
             ssl:{}:6643 and :6644 out of the OVN clusters by hand with `ovs-appctl … cluster/kick`",
            departed.address
        )];
    }
    let later =
        format!("run `orbivisorctl sdn central remove {name}` to kick it out of the OVN cluster");
    let sdn = match find_sdn(&state.cluster) {
        Ok(Some(s)) => s,
        _ => return vec![format!("node {name:?} held OVN central databases; {later}")],
    };
    let by = match live_central(&state.cluster, &sdn, departed.node) {
        Ok(Some(n)) => n,
        _ => {
            return vec![format!(
                "node {name:?} held OVN central databases and no OVN central is online; {later}"
            )];
        }
    };
    let id = departed.node;
    let check = |sdn: Option<&Sdn>| -> Result<(), ApiError> {
        let Some(sdn) = sdn.filter(|s| s.status.phase == SdnPhase::Ready) else {
            return Err(not_ready("SDN (OVN) is not set up"));
        };
        if !sdn.status.departed.iter().any(|d| d.node == id) {
            return Err(ApiError::bad_request("already kicked out"));
        }
        check_busy(&state.cluster, sdn, None)
    };
    let op = TaskOp::SdnCentralKick {
        node: id,
        address: departed.address,
    };
    match create_exclusive(state, op, by.id, check, |_| {}).await {
        Ok(task) => vec![format!(
            "node {name:?} held OVN central databases; node {:?} kicks it out of the OVN cluster (task {})",
            by.name, task.id
        )],
        Err(e) => vec![format!(
            "node {name:?} held OVN central databases; {later} ({})",
            e.message
        )],
    }
}

/// `PATCH /networks/{key}`: SDN 네트워크의 서브넷 목록 전체(계획 65 §9)
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NetworkPatch {
    /// 새 서브넷 목록. 이미 있는 서브넷(같은 `cidr`)은 `gateway`를 바꿀 수 없고 `dns_servers`·`pools`·`dhcp`·
    /// `description`을 바꾼다. 빠진 서브넷은 쓰는 NIC가 없어야 한다.
    pub subnets: Vec<Subnet>,
}

/// SDN 네트워크의 서브넷을 바꾼다(`If-Match`로 리비전 확인): 서브넷 더하기, `dns_servers`·`pools`·`dhcp` 바꾸기, 쓰는
/// NIC가 없는 서브넷 빼기. 서브넷을 더하면 그 네트워크에서 주소가 하나도 없는 NIC(켜진 VM 포함)가 새 서브넷의 주소를
/// 받는다(리뷰 2 N-4 — 서브넷이 있는 네트워크의 주소 없는 NIC는 OVN 포트가 꺼진다. 켜진 게스트는 다음 DHCP 갱신에서 받는다).
/// 그만큼 빈 주소가 없으면 409 `ip_exhausted`(아무것도 바꾸지 않음). 이미 주소가 있는 NIC에 빠진 체계는 채우지 않는다
/// (`orbivisorctl network ips NAME --fill`). 풀을 줄여도 이미 준 풀 밖 주소는 남는다. 단순 네트워크면 409
/// `network_immutable`, 서브넷 검증·`gateway` 바꾸기는 400 `invalid_subnet`, 빼는 서브넷의 주소를 쓰는 VM이 있으면 409
/// `subnet_in_use`(부른 사용자가 읽을 수 없는 VM은 수로만), 옛 노드가 남았으면 409 `cluster_upgrading`. 네트워크를 쓴 뒤
/// 채우지 못한 VM(그새 바뀜)은 응답 헤더 `orbivisor-warning`으로 알린다.
#[utoipa::path(patch, path = "/networks/{key}", tag = "networks",
    request_body = NetworkPatch,
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 200, body = Resource<NetworkSpec, NetworkStatus>,
        headers(("orbivisor-warning" = String, description = "interfaces that could not get an address (only then)"))),
        (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_network(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<NetworkPatch>,
) -> Result<(HeaderMap, Json<Network>), ApiError> {
    require_feature(&state.cluster, SDN)?;
    if req.subnets.iter().any(subnet_uses_routing) {
        return Err(routing_not_yet(
            &state.cluster,
            "floating_pools and ipv6_mode",
        ));
    }
    let network = lookup::<NetworkSpec>(&state.cluster, &key)?;
    if !network.spec.is_sdn() {
        return Err(ApiError::conflict(
            "network_immutable",
            format!(
                "network {:?} is not an SDN network; bridge and VXLAN networks cannot be changed (delete and create it again)",
                network.name
            ),
        ));
    }
    let mut spec = network.spec.clone();
    spec.subnets = req.subnets;
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()).with_code("invalid_subnet"))?;
    for new in &spec.subnets {
        if let Some(old) = network.spec.subnets.iter().find(|o| o.cidr == new.cidr)
            && old.gateway() != new.gateway()
        {
            return Err(ApiError::bad_request(format!(
                "subnet {}: the gateway cannot change ({} now); remove the subnet and add it again",
                new.cidr,
                old.gateway()
            ))
            .with_code("invalid_subnet"));
        }
    }
    let removed: Vec<&Subnet> = network
        .spec
        .subnets
        .iter()
        .filter(|o| !spec.subnets.iter().any(|n| n.cidr == o.cidr))
        .collect();
    let added: Vec<Subnet> = spec
        .subnets
        .iter()
        .filter(|n| !network.spec.subnets.iter().any(|o| o.cidr == n.cidr))
        .cloned()
        .collect();
    let (_, vms) = state.cluster.list::<VmSpec>()?;
    let sees = crate::ipam::readable(&state, Some(&principal));
    for subnet in &removed {
        let users: Vec<&opl_types::vm::Vm> = vms
            .iter()
            .filter(|vm| {
                vm.spec.nics.iter().any(|n| {
                    n.network == network.name && n.ips.iter().any(|ip| subnet.cidr.contains(*ip))
                })
            })
            .collect();
        if users.is_empty() {
            continue;
        }
        // 부른 사용자가 읽을 수 없는 VM(다른 폴더)은 이름을 대지 않는다(리뷰 2 m-1).
        let mut named: Vec<&str> = users
            .iter()
            .filter(|vm| sees(vm))
            .map(|vm| vm.name.as_str())
            .collect();
        let hidden = users.len() - named.len();
        let other = format!("{hidden} VM(s) you cannot see");
        if hidden > 0 {
            named.push(&other);
        }
        return Err(ApiError::conflict(
            "subnet_in_use",
            format!(
                "subnet {} of network {:?} has addresses of VMs {}; remove those NICs or change their addresses first",
                subnet.cidr,
                network.name,
                named.join(", ")
            ),
        ));
    }
    let (empty, short) = crate::ipam::fill_shortages(&vms, &network.name, &added);
    if !short.is_empty() {
        return Err(ApiError::conflict(
            "ip_exhausted",
            format!(
                "network {:?}: {empty} interface(s) have no address and would get one from the new subnets, \
                 but {} cannot give that many; use a larger subnet or pool, or give those interfaces addresses first",
                network.name,
                short.join(", ")
            ),
        ));
    }
    let cidrs: Vec<String> = spec.subnets.iter().map(|s| s.cidr.to_string()).collect();
    let revision = if_match(&headers)?.unwrap_or(network.revision);
    let written = state
        .cluster
        .update_spec(network.id, Some(revision), network.labels.clone(), &spec)
        .await?;
    let (filled, failed) = if empty > 0 && !added.is_empty() {
        Box::pin(fill_empty_nics(&state, &network.name, &added)).await
    } else {
        (0, Vec::new())
    };
    let mut detail = format!("network subnets {} [{}]", network.name, cidrs.join(", "));
    if filled > 0 {
        detail.push_str(&format!(", gave addresses to {filled} interface(s)"));
    }
    audit.detail(detail);
    let warning = (!failed.is_empty()).then(|| {
        format!(
            "could not give addresses to the interfaces of VMs {}; run `orbivisorctl network ips {} --fill`",
            failed.join(", "),
            network.name
        )
    });
    let mut out = network;
    out.spec = spec;
    out.revision = written;
    Ok((warning_header(warning), Json(out)))
}

/// 서브넷을 더한 뒤 네트워크 `network`의 주소 없는 NIC에 새 서브넷 `added`의 주소를 준다(VM마다 리비전 확인 쓰기 — 그새
/// 바뀌었거나 다른 노드가 같은 주소를 잡았으면 다시 고른다). 채운 NIC 수와 채우지 못한 VM 이름(까닭)을 돌려준다.
async fn fill_empty_nics(
    state: &AppState,
    network: &str,
    added: &[Subnet],
) -> (usize, Vec<String>) {
    let mut filled = 0;
    let mut failed = Vec::new();
    let ids: Vec<Uuid> = match state.cluster.list::<VmSpec>() {
        Ok((_, vms)) => vms
            .iter()
            .filter(|v| {
                !v.spec.template
                    && v.spec
                        .nics
                        .iter()
                        .any(|n| n.network == network && n.ips.is_empty())
            })
            .map(|v| v.id)
            .collect(),
        Err(e) => return (0, vec![format!("(cannot list VMs: {e})")]),
    };
    for id in ids {
        let mut done = false;
        let mut name = id.to_string();
        for _ in 0..crate::ipam::RETRIES * 2 {
            let _lock = state.vm_lock.lock().await;
            let vms = match state.cluster.list::<VmSpec>() {
                Ok((_, vms)) => vms,
                Err(e) => {
                    failed.push(format!("{name} ({e})"));
                    done = true;
                    break;
                }
            };
            let Some(vm) = vms.iter().find(|v| v.id == id) else {
                done = true;
                break;
            };
            name = vm.name.clone();
            let mut used = crate::ipam::network_used(&vms, network);
            let mut spec = vm.spec.clone();
            let count = match crate::ipam::fill_empty(&mut spec, network, added, &mut used) {
                Ok(0) => {
                    done = true;
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    failed.push(format!("{name} ({e})"));
                    done = true;
                    break;
                }
            };
            match state
                .cluster
                .update_spec(vm.id, Some(vm.revision), vm.labels.clone(), &spec)
                .await
            {
                Ok(_) => {
                    filled += count;
                    done = true;
                    break;
                }
                Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {}
                Err(e) if crate::ipam::is_ip_taken(&e) => {}
                Err(e) => {
                    failed.push(format!("{name} ({e})"));
                    done = true;
                    break;
                }
            }
        }
        if !done {
            failed.push(format!("{name} (it kept changing)"));
        }
    }
    (filled, failed)
}

/// 서브넷 하나의 크기와 사용
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SubnetUsage {
    #[schema(value_type = String)]
    pub cidr: String,
    /// 자동 할당 범위의 주소 수(예약 빼고, 아주 크면 u64 끝에서 멈춤)
    pub size: u64,
    /// 이 서브넷에서 NIC가 쓰는 주소 수(풀 밖 주소 포함)
    pub used: u64,
    /// 자동 할당 범위에서 남은 주소 수
    pub free: u64,
}

/// NIC 하나의 주소
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IpAllocation {
    pub vm: Uuid,
    pub vm_name: String,
    /// NIC 순번(0부터)
    pub nic: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub mac: Option<String>,
    #[schema(value_type = Vec<String>)]
    pub ips: Vec<IpAddr>,
    /// 논리 포트의 지금 상태(OVN에서 읽음). 읽지 못했으면 null
    pub live: Option<PortLive>,
}

/// OVN의 논리 포트 상태
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PortLive {
    /// 포트가 바인딩되어 흐름이 깔렸다(NB `up`).
    pub up: bool,
    /// 바인딩한 노드(이름, 모르는 chassis면 그 이름). 바인딩 전이거나 SB를 읽지 못했으면 없다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub node: Option<String>,
}

/// `GET /networks/{key}/ips`의 질의
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct IpsQuery {
    /// 거짓이면 OVN 포트 상태(`live`)를 읽지 않는다(목록의 사용률처럼 주소만 볼 때 — 그때 `live`는 모두 null이고
    /// `live_error`도 없다). 기본 참
    #[serde(default = "yes")]
    pub live: bool,
}

fn yes() -> bool {
    true
}

/// `GET /networks/{key}/ips`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NetworkIps {
    pub network: String,
    pub subnets: Vec<SubnetUsage>,
    /// 템플릿이 아닌 VM의 이 네트워크 NIC마다(VM 이름, NIC 순). 부른 사용자가 읽을 수 있는 VM의 것만 있다.
    pub allocations: Vec<IpAllocation>,
    /// 부른 사용자가 읽을 수 없는 VM(다른 폴더)의 NIC 수: 목록에서 빠졌지만 `subnets`의 사용 수에는 들었다.
    #[serde(default, skip_serializing_if = "is_zero")]
    #[schema(default)]
    pub hidden: usize,
    /// OVN을 읽지 못한 까닭(그때 `live`는 모두 null)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false, default)]
    pub live_error: Option<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// 주소의 비트(IPv4는 아래 32비트)
fn bits(ip: IpAddr) -> u128 {
    match ip.to_canonical() {
        IpAddr::V4(v) => u32::from(v).into(),
        IpAddr::V6(v) => v.into(),
    }
}

/// 자동 할당 범위(비트, 양 끝 포함). 풀이 없으면 CIDR에서 망 주소·브로드캐스트를 뺀 것이다.
fn ranges(subnet: &Subnet) -> Vec<(u128, u128)> {
    if !subnet.pools.is_empty() {
        return subnet
            .pools
            .iter()
            .map(|p| (bits(p.start), bits(p.end)))
            .collect();
    }
    let host = if subnet.is_ipv4() { 32 } else { 128 } - u32::from(subnet.cidr.prefix);
    let first = bits(subnet.cidr.addr);
    let last = first | ((1u128 << host) - 1);
    let last = if subnet.is_ipv4() { last - 1 } else { last };
    vec![(first + 1, last)]
}

/// 주소가 자동 할당 범위 안이고 예약이 아닌지
fn in_pool(subnet: &Subnet, ip: IpAddr) -> bool {
    let b = bits(ip);
    ip.to_canonical().is_ipv4() == subnet.is_ipv4()
        && ranges(subnet).iter().any(|(s, e)| (*s..=*e).contains(&b))
        && !ipam::reserved(subnet).contains(&ip.to_canonical())
}

/// 자동 할당 범위의 크기(예약 빼고)
fn pool_size(subnet: &Subnet) -> u128 {
    let ranges = ranges(subnet);
    let total = ranges
        .iter()
        .fold(0u128, |sum, (a, b)| sum.saturating_add(b - a + 1));
    let reserved = ipam::reserved(subnet)
        .into_iter()
        .filter(|ip| ranges.iter().any(|(s, e)| (*s..=*e).contains(&bits(*ip))))
        .count() as u128;
    total.saturating_sub(reserved)
}

/// SDN 네트워크의 주소: 서브넷마다 크기·사용·남음(모든 VM), NIC마다 VM·MAC·주소와 OVN 논리 포트의 `up`·바인딩
/// 노드(API 노드가 NB·SB 팔로어에서도 읽는다 — 못 읽으면 `live` null과 `live_error`). 네트워크 읽기 권한. NIC 목록은
/// 부른 사용자가 읽을 수 있는 VM의 것뿐이고 나머지는 `hidden` 수로만 알린다(리뷰 1 I-1 — 네트워크를 함께 쓰는 다른
/// 폴더의 VM 이름·주소를 드러내지 않게). `live=false`면 OVN을 읽지 않는다(목록의 사용률). 단순 네트워크면 409
/// `network_immutable`.
#[utoipa::path(get, path = "/networks/{key}/ips", tag = "networks",
    params(("key" = String, Path, description = "id or name"), IpsQuery),
    responses((status = 200, body = NetworkIps), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn network_ips(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    Query(query): Query<IpsQuery>,
) -> Result<Json<NetworkIps>, ApiError> {
    let network = lookup::<NetworkSpec>(&state.cluster, &key)?;
    if !network.spec.is_sdn() {
        return Err(ApiError::conflict(
            "network_immutable",
            format!(
                "network {:?} is not an SDN network and has no IPAM",
                network.name
            ),
        ));
    }
    let (_, mut vms) = state.cluster.list::<VmSpec>()?;
    vms.retain(|v| !v.spec.template);
    vms.sort_by(|a, b| a.name.cmp(&b.name));
    let mut allocations = Vec::new();
    for vm in &vms {
        for (i, nic) in vm.spec.nics.iter().enumerate() {
            if nic.network != network.name {
                continue;
            }
            allocations.push(IpAllocation {
                vm: vm.id,
                vm_name: vm.name.clone(),
                nic: i,
                mac: nic.mac.map(|m| m.to_string()),
                ips: nic.ips.clone(),
                live: None,
            });
        }
    }
    let subnets = network
        .spec
        .subnets
        .iter()
        .map(|s| {
            let size = pool_size(s);
            let used: Vec<IpAddr> = allocations
                .iter()
                .flat_map(|a| a.ips.iter().copied())
                .filter(|ip| s.cidr.contains(*ip))
                .collect();
            let in_pool = used.iter().filter(|ip| in_pool(s, **ip)).count() as u128;
            SubnetUsage {
                cidr: s.cidr.to_string(),
                size: u64::try_from(size).unwrap_or(u64::MAX),
                used: used.len() as u64,
                free: u64::try_from(size.saturating_sub(in_pool)).unwrap_or(u64::MAX),
            }
        })
        .collect();
    let readable: Vec<Uuid> = state
        .authz
        .filter(&principal, vms.iter())?
        .into_iter()
        .map(|v| v.id)
        .collect();
    let all = allocations.len();
    allocations.retain(|a| readable.contains(&a.vm));
    let hidden = all - allocations.len();
    let live_error = if allocations.is_empty() || !query.live {
        None
    } else {
        Box::pin(read_live(&state, &mut allocations)).await.err()
    };
    Ok(Json(NetworkIps {
        network: network.name,
        subnets,
        allocations,
        hidden,
        live_error,
    }))
}

/// 할당마다 OVN 논리 포트의 지금 상태를 채운다.
async fn read_live(state: &AppState, allocations: &mut [IpAllocation]) -> Result<(), String> {
    let Some(config) = &state.ovn else {
        return Err("this node does not read OVN".into());
    };
    let sdn = find_sdn(&state.cluster)
        .map_err(|e| e.message)?
        .filter(|s| s.status.phase == SdnPhase::Ready && !s.status.centrals.is_empty())
        .ok_or("SDN (OVN) is not ready")?;
    let (_, nodes) = state
        .cluster
        .list::<NodeSpec>()
        .map_err(|e| e.to_string())?;
    let centrals = crate::ovn_sync::central_hosts(&sdn, &nodes);
    let ports = crate::ovn_sync::read_ports(config, &centrals)
        .await
        .map_err(|e| e.to_string())?;
    let names: BTreeMap<String, &str> = nodes
        .iter()
        .map(|n| (n.id.to_string(), n.name.as_str()))
        .collect();
    for a in allocations.iter_mut() {
        let Ok(mac) = a.mac.as_deref().unwrap_or_default().parse() else {
            continue;
        };
        let port = opl_types::sdn::port_name(a.vm, mac);
        let found = ports.get(&port);
        a.live = Some(PortLive {
            up: found.is_some_and(|p| p.up),
            node: found
                .and_then(|p| p.chassis.as_ref())
                .map(|c| names.get(c).map_or_else(|| c.clone(), |n| (*n).to_string())),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subnet(json: serde_json::Value) -> Subnet {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn pool_size_leaves_out_reserved_addresses() {
        // /24: 망 주소·브로드캐스트 밖 254개에서 게이트웨이를 뺀다.
        assert_eq!(
            pool_size(&subnet(serde_json::json!({ "cidr": "10.0.0.0/24" }))),
            253
        );
        let pools = subnet(serde_json::json!({
            "cidr": "10.0.0.0/24", "pools": [{ "start": "10.0.0.10", "end": "10.0.0.19" }]
        }));
        assert_eq!(pool_size(&pools), 10);
        let v6 = subnet(serde_json::json!({ "cidr": "fd00::/64", "dhcp": false }));
        assert_eq!(pool_size(&v6), (1u128 << 64) - 2);
    }

    #[test]
    fn even_central_counts_warn() {
        assert!(even_warning(1).is_none());
        assert!(even_warning(3).is_none());
        assert!(even_warning(2).unwrap().contains("2 central nodes"));
        assert!(!warning_header(even_warning(4)).is_empty());
        assert!(warning_header(None).is_empty());
    }

    #[test]
    fn cert_names_follow_the_node_address_and_links() {
        let mut node: Node = Resource::new(
            "n1",
            NodeSpec {
                sdn: None,
                address: "10.0.0.5:8006".into(),
                maintenance: false,
                witness: false,
                roles: None,
                host_resources: None,
            },
        );
        node.status.cluster_addresses = vec!["10.1.0.5:8007".into()];
        assert_eq!(
            cert_names(&node),
            ["10.0.0.5", "10.1.0.5", "localhost", "127.0.0.1", "::1"]
        );
        // 루프백·지정 안 함·링크 로컬·멀티캐스트는 400(리뷰 1 I-5)
        for bad in [
            "127.0.0.1",
            "::1",
            "0.0.0.0",
            "169.254.0.5",
            "fe80::1",
            "224.0.0.1",
        ] {
            let err = central_ip(&node, Some(bad.parse().unwrap())).unwrap_err();
            assert_eq!(err.status, StatusCode::BAD_REQUEST, "{bad}");
        }
        // 노드 자신의 주소가 루프백이면(한 기계의 클러스터) 루프백이 그 노드의 주소다.
        let mut local = node.clone();
        local.spec.address = "127.0.0.1:8006".into();
        local.status.cluster_addresses.clear();
        assert_eq!(
            central_ip(&local, Some("127.0.0.1".parse().unwrap())).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert!(central_ip(&local, Some("::1".parse().unwrap())).is_err());
        // IPv4 대응 IPv6는 IPv4로 본다.
        assert_eq!(
            central_ip(&node, Some("::ffff:10.1.0.5".parse().unwrap())).unwrap(),
            "10.1.0.5".parse::<IpAddr>().unwrap()
        );
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(central_ip(&node, None).unwrap(), ip("10.0.0.5"));
        assert_eq!(
            central_ip(&node, Some(ip("10.1.0.5"))).unwrap(),
            ip("10.1.0.5")
        );
        let err = central_ip(&node, Some(ip("10.9.9.9"))).unwrap_err();
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
        assert!(
            err.message.contains("not in the certificate"),
            "{}",
            err.message
        );
    }
}
