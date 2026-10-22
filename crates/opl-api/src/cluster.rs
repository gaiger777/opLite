//! 클러스터 정보, 노드 합류(조인 토큰, CSR 서명), 노드 제거.
//!
//! 새 노드는 조인 URL을 받아 이 노드의 `POST /cluster/join`을 부른다. 토큰을 지워 한 번만 쓰게 하고,
//! 노드의 CSR에 클러스터 CA로 서명하고, Node 리소스를 만들고, 노드를 learner로 더한다. 새 노드는
//! 받은 인증서로 노드 간 서버를 띄우고 로그를 따라잡은 뒤 스스로 투표 멤버 승격을 요청한다.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use axum::response::IntoResponse;
use opl_cluster::ClusterError;
use opl_cluster::types::{LeaderRequest, NodeId};
use opl_peer::pki::{ClusterCa, fingerprint};
use opl_types::cluster::{ClusterInfo, ClusterSpec, JoinToken, JoinTokenSpec};
use opl_types::node::{NodeSpec, NodeStatus};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal, constant_time_eq, random_hex, sha256_hex};
use crate::error::{ApiError, Json};
use crate::resources::{check_name, lookup};
use crate::state::AppState;

/// 조인 토큰의 기본 유효 시간
pub const DEFAULT_JOIN_TTL: Duration = Duration::from_secs(3600);
/// 조인 토큰의 최대 유효 시간
const MAX_JOIN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const JOIN_TOKEN_PREFIX: &str = "oplj_";
const JOIN_SCHEME: &str = "opl-join://";

/// `opl-join://<API 주소>?token=<토큰>&ca=<CA 지문>`. `oplctl cluster join`에 그대로 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinUrl {
    /// 합류 요청을 받을 노드의 API 주소(`host:port`)
    pub address: String,
    pub token: String,
    /// 클러스터 CA 인증서의 지문(`sha256:<16진수>`). 새 노드는 이것으로 상대를 확인한다.
    pub ca_fingerprint: String,
}

impl fmt::Display for JoinUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{JOIN_SCHEME}{}?token={}&ca={}",
            self.address, self.token, self.ca_fingerprint
        )
    }
}

impl FromStr for JoinUrl {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || {
            format!(
                "invalid join URL {s:?}: expected {JOIN_SCHEME}<host:port>?token=...&ca=sha256:..."
            )
        };
        let rest = s.trim().strip_prefix(JOIN_SCHEME).ok_or_else(bad)?;
        let (address, query) = rest.split_once('?').ok_or_else(bad)?;
        let (mut token, mut ca) = (None, None);
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("token", v)) => token = Some(v.to_string()),
                Some(("ca", v)) => ca = Some(v.to_string()),
                _ => return Err(bad()),
            }
        }
        if host_of(address).is_none() {
            return Err(bad());
        }
        Ok(JoinUrl {
            address: address.to_string(),
            token: token.ok_or_else(bad)?,
            ca_fingerprint: ca.filter(|c| c.starts_with("sha256:")).ok_or_else(bad)?,
        })
    }
}

/// `host:port`나 `[v6]:port`의 호스트. 포트가 숫자가 아니면 `None`이다.
pub fn host_of(address: &str) -> Option<&str> {
    let (host, port) = address.rsplit_once(':')?;
    port.parse::<u16>().ok()?;
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 클러스터 정보 리소스. 클러스터를 만들 때 하나 생긴다.
fn cluster_info(state: &AppState) -> Result<ClusterInfo, ApiError> {
    state
        .cluster
        .list::<ClusterSpec>()?
        .1
        .into_iter()
        .next()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_not_initialized",
                "the cluster has not been initialized",
            )
        })
}

fn ca_fingerprint(info: &ClusterInfo) -> Result<String, ApiError> {
    fingerprint(&info.spec.ca_cert_pem).map_err(ApiError::internal)
}

/// 클러스터 멤버 한 노드
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct MemberView {
    pub id: Uuid,
    /// Node 리소스 이름. 아직 없으면(합류 중) 비어 있다.
    pub name: Option<String>,
    /// API 주소
    pub address: Option<String>,
    /// 노드 간 통신 주소
    pub cluster_address: String,
    /// 투표 멤버인지(아니면 learner)
    pub voter: bool,
    pub online: bool,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ClusterView {
    pub name: String,
    /// 클러스터 CA 인증서의 지문. `oplctl login --ca-fingerprint`에 쓴다.
    pub ca_fingerprint: String,
    pub leader: Option<Uuid>,
    pub members: Vec<MemberView>,
}

/// 클러스터 이름, CA 지문, 멤버(이 노드가 아는 Raft 멤버십)
#[utoipa::path(get, path = "/cluster", tag = "cluster",
    responses((status = 200, body = ClusterView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_cluster(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<ClusterView>, ApiError> {
    state.authorize(&principal, Action::Read, ClusterSpec::KIND)?;
    let info = cluster_info(&state)?;
    let members = state.cluster.members();
    let nodes = state.cluster.list::<NodeSpec>()?.1;
    let view = |id: NodeId, addr: &String, voter: bool| {
        let node = nodes.iter().find(|n| n.id == id.0);
        MemberView {
            id: id.0,
            name: node.map(|n| n.name.clone()),
            address: node.map(|n| n.spec.address.clone()),
            cluster_address: addr.clone(),
            voter,
            online: node.is_some_and(|n| n.status.online),
        }
    };
    let mut list: Vec<MemberView> = members
        .voters
        .iter()
        .map(|(id, addr)| view(*id, addr, true))
        .collect();
    list.extend(
        members
            .learners
            .iter()
            .map(|(id, addr)| view(*id, addr, false)),
    );
    Ok(Json(ClusterView {
        name: info.name.clone(),
        ca_fingerprint: ca_fingerprint(&info)?,
        leader: members.leader.map(|id| id.0),
        members: list,
    }))
}

/// 클러스터 CA 인증서(PEM). 인증 없이 읽는다. 받는 쪽은 반드시 지문을 확인해야 한다.
#[utoipa::path(get, path = "/cluster/ca", tag = "cluster", security(()),
    responses((status = 200, description = "CA certificate", content_type = "application/x-pem-file", body = String),
              (status = "5XX", body = crate::error::ErrorBody)))]
pub async fn get_ca(State(state): State<Arc<AppState>>) -> Result<impl IntoResponse, ApiError> {
    let info = cluster_info(&state)?;
    Ok((
        [(CONTENT_TYPE, "application/x-pem-file")],
        info.spec.ca_cert_pem,
    ))
}

#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenRequest {
    /// 유효 시간(초). 기본 1시간, 최대 7일
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CreatedJoinToken {
    /// 새 노드에서 `oplctl cluster join`에 넘길 URL. 다시 볼 수 없다.
    pub url: String,
    /// 만료 시각(유닉스 초)
    pub expires_at: u64,
}

/// 새 토큰과 그 조인 URL(이 노드의 API 주소). 토큰은 한 번만 쓸 수 있다.
pub async fn issue_join_token(
    state: &AppState,
    ttl: Duration,
) -> Result<CreatedJoinToken, ApiError> {
    let info = cluster_info(state)?;
    let me = state
        .cluster
        .get::<NodeSpec>(state.node)?
        .ok_or_else(|| ApiError::internal("this node has no Node resource"))?;
    let secret = random_hex(32);
    let expires_at = now_secs() + ttl.as_secs();
    let mut token: JoinToken = Resource::new(
        "",
        JoinTokenSpec {
            secret_hash: sha256_hex(&secret),
            expires_at,
        },
    );
    token.name = token.id.to_string();
    state.cluster.create(&token).await?;
    let url = JoinUrl {
        address: me.spec.address,
        token: format!("{JOIN_TOKEN_PREFIX}{}_{secret}", token.id.simple()),
        ca_fingerprint: ca_fingerprint(&info)?,
    };
    Ok(CreatedJoinToken {
        url: url.to_string(),
        expires_at,
    })
}

#[utoipa::path(post, path = "/cluster/join-tokens", tag = "cluster",
    request_body = JoinTokenRequest,
    responses((status = 201, body = CreatedJoinToken), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_join_token(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<JoinTokenRequest>,
) -> Result<(StatusCode, Json<CreatedJoinToken>), ApiError> {
    state.authorize(&principal, Action::Write, JoinTokenSpec::KIND)?;
    let ttl = req.ttl_secs.map_or(DEFAULT_JOIN_TTL, Duration::from_secs);
    if ttl.is_zero() || ttl > MAX_JOIN_TTL {
        return Err(ApiError::bad_request(
            "ttl_secs must be between 1 and 604800",
        ));
    }
    Ok((
        StatusCode::CREATED,
        Json(issue_join_token(&state, ttl).await?),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinRequest {
    /// 조인 URL의 토큰
    pub token: String,
    /// 새 노드의 id(Raft 노드 id, Node 리소스 id)
    pub node: Uuid,
    /// 새 노드의 이름
    pub name: String,
    /// 새 노드의 API 주소(`host:port`)
    pub address: String,
    /// 새 노드의 노드 간 통신 주소(`host:port`)
    pub cluster_address: String,
    /// 새 노드가 만든 키의 CSR(PEM)
    pub csr_pem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JoinResponse {
    pub cluster: String,
    pub ca_pem: String,
    /// 새 노드의 인증서(PEM)
    pub cert_pem: String,
    /// 이 노드의 노드 간 통신 주소. 새 노드는 여기로 승격을 요청한다.
    pub via: String,
}

fn invalid_token() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_token",
        "invalid or expired join token",
    )
}

/// 새 노드의 합류 요청. 조인 토큰으로 인증한다(사용자 인증 없음).
#[utoipa::path(post, path = "/cluster/join", tag = "cluster", security(()),
    request_body = JoinRequest,
    responses((status = 200, body = JoinResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn join(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, ApiError> {
    // 토큰부터 확인해, 토큰 없는 요청에는 클러스터에 대해 아무것도 알려 주지 않는다.
    let (id, secret) = req
        .token
        .strip_prefix(JOIN_TOKEN_PREFIX)
        .and_then(|rest| rest.split_once('_'))
        .and_then(|(id, secret)| Some((Uuid::try_parse(id).ok()?, secret)))
        .ok_or_else(invalid_token)?;
    let token = state
        .cluster
        .get::<JoinTokenSpec>(id)?
        .ok_or_else(invalid_token)?;
    if !constant_time_eq(&sha256_hex(secret), &token.spec.secret_hash)
        || token.spec.expires_at <= now_secs()
    {
        return Err(invalid_token());
    }

    check_name(&req.name)?;
    let (Some(api_host), Some(cluster_host)) =
        (host_of(&req.address), host_of(&req.cluster_address))
    else {
        return Err(ApiError::bad_request(
            "address and cluster_address must be host:port",
        ));
    };
    if req.node.is_nil() {
        return Err(ApiError::bad_request("node id must not be nil"));
    }
    // learner이면 합류하다 멈춘 노드다. 새 토큰으로 다시 합류할 수 있다.
    if state
        .cluster
        .members()
        .voters
        .contains_key(&NodeId(req.node))
    {
        return Err(ApiError::conflict(
            "already_member",
            format!("node {} is already a cluster member", req.node),
        ));
    }
    if let Some(other) = state.cluster.get_by_name::<NodeSpec>(&req.name)?
        && other.id != req.node
    {
        return Err(ApiError::conflict(
            "name_taken",
            format!("node name {:?} is already taken", req.name),
        ));
    }
    let info = cluster_info(&state)?;
    let me = state
        .cluster
        .members()
        .addr(NodeId(state.node))
        .map(str::to_string)
        .ok_or_else(|| ApiError::internal("this node is not in the membership it knows"))?;

    let mut sans = vec![api_host.to_string()];
    if cluster_host != api_host {
        sans.push(cluster_host.to_string());
    }
    sans.extend(["localhost".into(), "127.0.0.1".into(), "::1".into()]);
    let ca = ClusterCa {
        cert_pem: info.spec.ca_cert_pem.clone(),
        key_pem: info.spec.ca_key_pem.clone(),
    };
    let cert_pem = ca
        .sign_node(&req.csr_pem, req.node, &sans)
        .map_err(|e| ApiError::bad_request(format!("cannot sign the CSR: {e}")))?;

    // 토큰을 지운다. 동시에 같은 토큰을 쓴 요청은 하나만 성공한다.
    match state
        .cluster
        .delete::<JoinTokenSpec>(token.id, Some(token.revision))
        .await
    {
        Ok(_) => {}
        Err(ClusterError::Rejected(_)) => return Err(invalid_token()),
        Err(e) => return Err(e.into()),
    }

    // 앞서 합류하다 실패한 같은 노드면 Node 리소스를 다시 쓴다.
    let spec = NodeSpec {
        address: req.address.clone(),
        maintenance: false,
    };
    match state.cluster.get::<NodeSpec>(req.node)? {
        Some(node) => {
            state
                .cluster
                .update_spec(node.id, None, node.labels, &spec)
                .await?;
        }
        None => {
            let mut node: Resource<NodeSpec, NodeStatus> = Resource::new(req.name.clone(), spec);
            node.id = req.node;
            state.cluster.create(&node).await?;
        }
    }
    state
        .cluster
        .request(LeaderRequest::AddLearner {
            id: NodeId(req.node),
            addr: req.cluster_address.clone(),
        })
        .await?;
    tracing::info!(node = %req.name, id = %req.node, "node joined as learner");
    Ok(Json(JoinResponse {
        cluster: info.name,
        ca_pem: info.spec.ca_cert_pem,
        cert_pem,
        via: me,
    }))
}

/// 죽은 노드를 클러스터에서 뺀다. 살아 있는 노드는 그 노드에서 `oplctl cluster leave`로 뺀다.
/// 그 노드에 배정된 VM은 그대로 남는다.
#[utoipa::path(delete, path = "/nodes/{key}", tag = "nodes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 204), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_node(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NodeSpec::KIND)?;
    let node = lookup::<NodeSpec>(&state.cluster, &key)?;
    if node.id == state.node || node.status.online {
        return Err(ApiError::conflict(
            "node_online",
            format!(
                "node {:?} is online; run `oplctl cluster leave` on it instead",
                node.name
            ),
        ));
    }
    state
        .cluster
        .request(LeaderRequest::Remove {
            id: NodeId(node.id),
        })
        .await?;
    match state.cluster.delete::<NodeSpec>(node.id, None).await {
        Ok(_) | Err(ClusterError::Rejected(_)) => {}
        Err(e) => return Err(e.into()),
    }
    tracing::info!(node = %node.name, "node removed from the cluster");
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_round_trips() {
        let url = JoinUrl {
            address: "[::1]:8006".into(),
            token: "oplj_abc_def".into(),
            ca_fingerprint: "sha256:00ff".into(),
        };
        let s = url.to_string();
        assert_eq!(s, "opl-join://[::1]:8006?token=oplj_abc_def&ca=sha256:00ff");
        assert_eq!(s.parse::<JoinUrl>().unwrap(), url);
        for bad in [
            "https://h:1?token=a&ca=sha256:00",
            "opl-join://h?token=a&ca=sha256:00",
            "opl-join://h:1?token=a",
            "opl-join://h:1?token=a&ca=md5:00",
            "opl-join://h:1?token=a&ca=sha256:00&x=1",
        ] {
            assert!(bad.parse::<JoinUrl>().is_err(), "{bad}");
        }
    }

    #[test]
    fn host_of_handles_v6_and_names() {
        assert_eq!(host_of("10.0.0.5:8006"), Some("10.0.0.5"));
        assert_eq!(host_of("[::1]:8006"), Some("::1"));
        assert_eq!(host_of("node1.lan:8007"), Some("node1.lan"));
        assert_eq!(host_of("node1.lan"), None);
        assert_eq!(host_of(":8006"), None);
        assert_eq!(host_of("h:port"), None);
    }
}
