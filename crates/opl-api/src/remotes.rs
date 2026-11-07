//! 원격 클러스터 `/api/v1/remotes`와 리더가 원격의 상태를 모으는 [`RemotePoller`]. 원격은 누구나
//! 읽고 admin만 바꾼다. 저장한 API 토큰은 응답에 넣지 않고 토큰의 id(`token_id`)만 보인다. 원격은
//! watch로도 내보내지 않는다(토큰을 담고 있다).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::remote::{
    NodeCounts, Remote, RemoteSpec, RemoteStatus, VmCounts, normalize_url, token_id,
};
use opl_types::resource::{Resource, ResourceSpec};
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::remote_client::{RemoteClient, RemoteError, fetch_ca};
use crate::resources::{List, check_name, if_match, lookup};
use crate::state::AppState;

/// 원격 하나의 상태를 모으는 시간 제한
const COLLECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 만들기·바꾸기 전에 원격을 확인하는 시간 제한(전체)
const VERIFY_TIMEOUT: Duration = Duration::from_secs(30);

/// 원격 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되, `token`을 빼면 저장된 토큰을 그대로 둔다
/// (`url`·`ca_fingerprint`가 모두 그대로일 때만 — 하나라도 바뀌면 400). 저장하기 전에 원격에 연결해
/// CA 지문과 토큰을 확인한다(30초 제한). 확인한 CA 인증서는 저장해 두고 연결할 때 다시 받지 않으며,
/// `url`이나 `ca_fingerprint`가 바뀔 때만 다시 받는다.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteRequest {
    /// 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    /// `https://<호스트>[:포트]`
    pub url: String,
    /// 원격 클러스터 CA의 지문(`sha256:<16진수>` — 대문자·`:` 구분도 받는다)
    pub ca_fingerprint: String,
    /// 원격 클러스터에서 만든 API 토큰. 만들 때 필요하다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub token: Option<String>,
}

// 토큰이 로그에 나가지 않게 가린다.
impl std::fmt::Debug for RemoteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteRequest")
            .field("name", &self.name)
            .field("url", &self.url)
            .field("ca_fingerprint", &self.ca_fingerprint)
            .field("token", &"***")
            .finish()
    }
}

/// 응답의 원격. 토큰 대신 토큰의 id만 보인다(원격에서 어느 토큰인지 알아보게).
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RemoteView {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub url: String,
    pub ca_fingerprint: String,
    /// 저장한 API 토큰의 id
    #[schema(required = true)]
    pub token_id: Option<Uuid>,
    pub status: RemoteStatus,
}

impl From<Remote> for RemoteView {
    fn from(r: Remote) -> Self {
        Self {
            id: r.id,
            name: r.name,
            revision: r.revision,
            token_id: token_id(&r.spec.token),
            url: r.spec.url,
            ca_fingerprint: r.spec.ca_fingerprint,
            status: r.status,
        }
    }
}

pub(crate) fn remote_unreachable(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "remote_unreachable", e.to_string())
}

/// 저장하기 전에 원격을 확인한다(전체 30초, 넘으면 502 `remote_unreachable`). `spec.ca_pem`이 비었으면
/// CA를 받아(실패 502 `remote_unreachable`) 지문을 비교하고(다르면 400 `remote_fingerprint_mismatch`),
/// 이 클러스터 자신이 아닌지 보고(400 `remote_is_local`), `spec.ca_pem`에 둔다. 그 CA로 토큰을 써
/// `GET /auth/whoami`를 읽는다(401이면 400 `remote_unauthorized`, 그 밖의 실패는 502).
async fn verify(state: &AppState, spec: &mut RemoteSpec) -> Result<(), ApiError> {
    tokio::time::timeout(VERIFY_TIMEOUT, verify_now(state, spec))
        .await
        .unwrap_or_else(|_| Err(remote_unreachable("timed out checking the remote cluster")))
}

async fn verify_now(state: &AppState, spec: &mut RemoteSpec) -> Result<(), ApiError> {
    if spec.ca_pem.is_empty() {
        let pem = fetch_ca(&spec.url).await.map_err(remote_unreachable)?;
        let actual = opl_peer::pki::fingerprint(&pem).map_err(remote_unreachable)?;
        if actual != spec.ca_fingerprint {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "remote_fingerprint_mismatch",
                format!(
                    "the remote CA fingerprint is {actual}, not {}",
                    spec.ca_fingerprint
                ),
            ));
        }
        let local = crate::cluster::cluster_info(state)?;
        let local =
            opl_peer::pki::fingerprint(&local.spec.ca_cert_pem).map_err(ApiError::internal)?;
        if actual == local {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "remote_is_local",
                "the remote is this cluster",
            ));
        }
        spec.ca_pem = pem;
    }
    let client =
        RemoteClient::new(&spec.url, &spec.ca_pem, &spec.token).map_err(remote_unreachable)?;
    match client.get_json::<serde_json::Value>("/auth/whoami").await {
        Ok(_) => Ok(()),
        Err(RemoteError::Status(401, message)) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "remote_unauthorized",
            format!("the remote cluster rejected the token: {message}"),
        )),
        Err(e) => Err(remote_unreachable(e)),
    }
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 토큰을 채운다). 연결 대상(`url`·
/// `ca_fingerprint`)이 그대로면 확인한 CA도 그대로 두고, 아니면 비워 [`verify`]가 다시 받게 한다.
fn spec_of(req: &RemoteRequest, stored: Option<&RemoteSpec>) -> Result<RemoteSpec, ApiError> {
    let bad = |e: opl_types::remote::RemoteSpecError| ApiError::bad_request(e.to_string());
    let url = normalize_url(&req.url).map_err(bad)?;
    let ca_fingerprint = opl_types::remote::normalize_ca_fingerprint(&req.ca_fingerprint)
        .ok_or(opl_types::remote::RemoteSpecError::Fingerprint)
        .map_err(bad)?;
    let same = stored.filter(|old| old.url == url && old.ca_fingerprint == ca_fingerprint);
    let token = match (&req.token, stored) {
        (Some(token), _) => token.clone(),
        // 저장된 토큰을 다른 곳으로 보내지 않는다.
        (None, Some(old)) if same.is_some() => old.token.clone(),
        (None, Some(_)) => {
            return Err(ApiError::bad_request(
                "token must be given again when url or ca_fingerprint changes",
            ));
        }
        (None, None) => return Err(ApiError::bad_request("token is required")),
    };
    let mut spec = RemoteSpec {
        url,
        ca_fingerprint,
        token,
        ca_pem: same.map(|old| old.ca_pem.clone()).unwrap_or_default(),
    };
    spec.normalize().map_err(bad)?;
    Ok(spec)
}

#[utoipa::path(get, path = "/remotes", tag = "remotes",
    responses((status = 200, body = List<RemoteView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_remotes(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<RemoteView>>, ApiError> {
    state.authorize(&principal, Action::Read, RemoteSpec::KIND)?;
    let (revision, remotes) = state.cluster.list::<RemoteSpec>()?;
    Ok(Json(List {
        revision,
        items: remotes.into_iter().map(RemoteView::from).collect(),
    }))
}

/// 원격을 확인하고([`verify`]) 저장한 뒤 곧 상태를 한 번 모은다. 나쁜 값은 400 `invalid_request`.
#[utoipa::path(post, path = "/remotes", tag = "remotes",
    request_body = RemoteRequest,
    responses((status = 201, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn create_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RemoteRequest>,
) -> Result<(StatusCode, Json<RemoteView>), ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let name = req
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    check_name(&name)?;
    // `/remotes/probe`는 경로가 따로 있어 이 이름으로는 읽을 수 없다.
    if name == "probe" {
        return Err(ApiError::bad_request("a remote cannot be named \"probe\""));
    }
    let mut spec = spec_of(&req, None)?;
    // 옛 노드는 새 종류를 풀지 못한다.
    crate::cluster::require_current(&state.cluster)?;
    verify(&state, &mut spec).await?;
    let mut remote: Remote = Resource::new(name, spec);
    remote.revision = state.cluster.create(&remote).await?;
    let remote = refresh(&state, remote).await;
    Ok((StatusCode::CREATED, Json(remote.into())))
}

#[utoipa::path(get, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Read, RemoteSpec::KIND)?;
    Ok(Json(lookup::<RemoteSpec>(&state.cluster, &key)?.into()))
}

/// 원격을 통째로 바꾼다(`token` 규칙은 [`RemoteRequest`]). 바뀌면 다시 확인하고 상태를 곧 모은다.
/// 확인하는 사이에 상태만 바뀌었으면(수집기 따위) 다시 확인하지 않고 새 revision에 쓴다(`If-Match`도
/// 그대로 통과). spec이 바뀌었으면 `If-Match`가 있으면 412, 없으면 다시 읽어 바꾼다.
#[utoipa::path(put, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = RemoteRequest,
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn update_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(req): Json<RemoteRequest>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let expected = if_match(&headers)?;
    // 확인하기 전에 읽은 spec과 확인한 spec(쓰지 못했을 때)
    let mut verified: Option<(RemoteSpec, RemoteSpec)> = None;
    for _ in 0..10 {
        let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
        let spec = match verified.take() {
            Some((read, spec)) if read == remote.spec => spec,
            _ => {
                if let Some(name) = &req.name
                    && *name != remote.name
                {
                    return Err(ApiError::bad_request(
                        "the name of a remote cannot be changed",
                    ));
                }
                if let Some(expected) = expected
                    && expected != remote.revision
                {
                    return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                        expected,
                        actual: remote.revision,
                    })
                    .into());
                }
                let mut spec = spec_of(&req, Some(&remote.spec))?;
                if spec == remote.spec {
                    return Ok(Json(remote.into()));
                }
                crate::cluster::require_current(&state.cluster)?;
                verify(&state, &mut spec).await?;
                spec
            }
        };
        let revision = match state
            .cluster
            .update_spec(
                remote.id,
                Some(remote.revision),
                remote.labels.clone(),
                &spec,
            )
            .await
        {
            Ok(revision) => revision,
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => {
                verified = Some((remote.spec, spec));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        let updated = Remote {
            revision,
            spec,
            ..remote
        };
        return Ok(Json(refresh(&state, updated).await.into()));
    }
    Err(ApiError::conflict(
        "busy",
        "the remote kept changing while being updated; try again",
    ))
}

#[utoipa::path(delete, path = "/remotes/{key}", tag = "remotes",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    state
        .cluster
        .delete::<RemoteSpec>(remote.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 원격 상태를 곧 다시 모아 적고 돌려준다.
#[utoipa::path(post, path = "/remotes/{key}/actions/refresh", tag = "remotes",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = RemoteView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn refresh_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<RemoteView>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let remote = lookup::<RemoteSpec>(&state.cluster, &key)?;
    crate::cluster::require_current(&state.cluster)?;
    Ok(Json(refresh(&state, remote).await.into()))
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoteProbeRequest {
    /// `https://<호스트>[:포트]`
    pub url: String,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct RemoteProbe {
    /// 정규화한 URL
    pub url: String,
    /// 받은 CA 인증서의 지문. 원격 클러스터에서 보이는 지문과 같은지 사람이 확인해야 한다.
    pub ca_fingerprint: String,
}

/// 원격의 CA를 인증서를 검증하지 않고 받아 지문을 돌려준다(웹이 등록 전에 보여 주고 확인받는다).
/// 30초 안에 받지 못하면 502 `remote_unreachable`.
#[utoipa::path(post, path = "/remotes/probe", tag = "remotes",
    request_body = RemoteProbeRequest,
    responses((status = 200, body = RemoteProbe), (status = "4XX", body = crate::error::ErrorBody),
              (status = 502, body = crate::error::ErrorBody)))]
pub async fn probe_remote(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Json(req): Json<RemoteProbeRequest>,
) -> Result<Json<RemoteProbe>, ApiError> {
    state.authorize(&principal, Action::Write, RemoteSpec::KIND)?;
    let url = normalize_url(&req.url).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let pem = tokio::time::timeout(VERIFY_TIMEOUT, fetch_ca(&url))
        .await
        .map_err(|_| remote_unreachable("timed out checking the remote cluster"))?
        .map_err(remote_unreachable)?;
    let ca_fingerprint = opl_peer::pki::fingerprint(&pem).map_err(remote_unreachable)?;
    Ok(Json(RemoteProbe {
        url,
        ca_fingerprint,
    }))
}

/// 상태를 모아(10초 제한) 바뀌었으면 적고 원격을 돌려준다.
async fn refresh(state: &AppState, remote: Remote) -> Remote {
    let collected = collect_remote(&remote).await;
    recorded(&state.cluster, remote, collected).await
}

/// 모은 상태를 [`record`]하고 원격을 돌려준다. 적지 않았으면(같거나 그 사이 다른 쓰기가 있었으면) 저장된
/// 것을 다시 읽어 돌려준다(지워졌으면 가진 것). 적지 못하면 로그만 남긴다.
async fn recorded(cluster: &Cluster, mut remote: Remote, collected: RemoteStatus) -> Remote {
    match record(cluster, &remote, collected).await {
        Ok(Some((revision, status))) => {
            remote.revision = revision;
            remote.status = status;
            remote
        }
        Ok(None) => cluster
            .get::<RemoteSpec>(remote.id)
            .ok()
            .flatten()
            .unwrap_or(remote),
        Err(e) => {
            tracing::warn!(remote = %remote.name, error = %e, "cannot record the remote status");
            remote
        }
    }
}

/// 저장한 CA로 연결해 상태를 모은다(10초 제한).
async fn collect_remote(remote: &Remote) -> RemoteStatus {
    let work = async {
        match RemoteClient::new(&remote.spec.url, &remote.spec.ca_pem, &remote.spec.token) {
            Ok(client) => collect(&client).await,
            Err(e) => unreachable_status(&e),
        }
    };
    tokio::time::timeout(COLLECT_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| unreachable_status(&"timed out collecting the status"))
}

/// 모은 상태가 저장된 것과 다르면 `checked_at`을 지금으로 해 적는다(같으면 쓰지 않는다 — Raft 부하).
/// 그 사이 원격이 바뀌었으면 쓰지 않는다. 적었으면 새 revision과 상태.
async fn record(
    cluster: &Cluster,
    remote: &Remote,
    mut status: RemoteStatus,
) -> Result<Option<(u64, RemoteStatus)>, ClusterError> {
    status.checked_at = remote.status.checked_at;
    if status == remote.status {
        return Ok(None);
    }
    status.checked_at = Some(now());
    match cluster
        .update_status_checked::<RemoteSpec>(remote.id, remote.revision, &status)
        .await
    {
        Ok(revision) => Ok(Some((revision, status))),
        Err(ClusterError::Rejected(
            Rejection::RevisionMismatch { .. } | Rejection::NotFound { .. },
        )) => Ok(None),
        Err(e) => Err(e),
    }
}

fn unreachable_status(e: &dyn std::fmt::Display) -> RemoteStatus {
    RemoteStatus {
        reachable: false,
        error: Some(e.to_string()),
        ..RemoteStatus::default()
    }
}

/// 원격이 돌려주는 목록(필요한 필드만 읽는다 — 원격의 버전이 달라도 읽히게)
#[derive(Deserialize)]
struct Items<T> {
    items: Vec<T>,
}

#[derive(Deserialize)]
struct ClusterName {
    name: String,
}

#[derive(Deserialize)]
struct RemoteNode {
    #[serde(default)]
    spec: RemoteNodeSpec,
    #[serde(default)]
    status: RemoteNodeStatus,
}

#[derive(Default, Deserialize)]
struct RemoteNodeSpec {
    #[serde(default)]
    witness: bool,
}

#[derive(Default, Deserialize)]
struct RemoteNodeStatus {
    #[serde(default)]
    online: bool,
    #[serde(default)]
    cpus: u64,
    #[serde(default)]
    memory_mib: u64,
    #[serde(default)]
    version: Option<String>,
}

#[derive(Deserialize)]
struct RemoteVm {
    #[serde(default)]
    status: RemoteVmStatus,
}

#[derive(Default, Deserialize)]
struct RemoteVmStatus {
    #[serde(default)]
    phase: String,
}

/// `GET /cluster`, `/nodes`, `/vms`를 읽어 상태를 계산한다(`checked_at`은 비운다).
async fn collect(client: &RemoteClient) -> RemoteStatus {
    let read = async {
        let cluster: ClusterName = client.get_json("/cluster").await?;
        let nodes: Items<RemoteNode> = client.get_json("/nodes").await?;
        let vms: Items<RemoteVm> = client.get_json("/vms").await?;
        Ok::<_, RemoteError>(summarize(cluster.name, &nodes.items, &vms.items))
    };
    read.await.unwrap_or_else(|e| unreachable_status(&e))
}

/// 노드 수·CPU·메모리는 위트니스를 빼고, 버전은 위트니스를 포함한 노드 가운데 가장 낮은 것이다.
fn summarize(cluster_name: String, nodes: &[RemoteNode], vms: &[RemoteVm]) -> RemoteStatus {
    let version = nodes
        .iter()
        .filter_map(|n| n.status.version.as_deref())
        .min_by(|a, b| opl_types::version::compare(a, b))
        .map(str::to_string);
    let hosts: Vec<&RemoteNode> = nodes.iter().filter(|n| !n.spec.witness).collect();
    RemoteStatus {
        reachable: true,
        checked_at: None,
        error: None,
        cluster_name: Some(cluster_name),
        version,
        nodes: Some(NodeCounts {
            total: hosts.len() as u64,
            online: hosts.iter().filter(|n| n.status.online).count() as u64,
        }),
        vms: Some(VmCounts {
            total: vms.len() as u64,
            running: vms.iter().filter(|v| v.status.phase == "running").count() as u64,
        }),
        cpus: Some(hosts.iter().map(|n| n.status.cpus).sum()),
        memory_gib: Some(hosts.iter().map(|n| n.status.memory_mib).sum::<u64>() / 1024),
    }
}

/// 원격 상태 수집 설정
#[derive(Debug, Clone)]
pub struct RemotePollerConfig {
    /// 모을 때가 됐는지 확인하는 간격
    pub tick: Duration,
    /// 원격마다 상태를 모으는 간격
    pub interval: Duration,
}

impl Default for RemotePollerConfig {
    fn default() -> Self {
        Self {
            tick: Duration::from_secs(5),
            interval: Duration::from_secs(30),
        }
    }
}

/// 원격 상태 수집 반복 작업. 모든 노드에서 띄우고, 리더인 노드에서만 모은다.
pub struct RemotePoller {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RemotePoller {
    pub fn spawn(state: Arc<AppState>, config: RemotePollerConfig) -> Self {
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

async fn run(
    state: Arc<AppState>,
    config: RemotePollerConfig,
    mut shutdown: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(config.tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // 원격마다 마지막으로 모은 때(리더의 메모리에만)
    let mut polled: HashMap<Uuid, Instant> = HashMap::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => return,
            _ = interval.tick() => {}
        }
        tokio::select! {
            _ = &mut shutdown => return,
            _ = tick(&state, &config, &mut polled) => {}
        }
    }
}

async fn tick(state: &AppState, config: &RemotePollerConfig, polled: &mut HashMap<Uuid, Instant>) {
    let cluster = &state.cluster;
    if cluster.members().leader != Some(cluster.id()) {
        // 다시 리더가 되면 곧 모은다.
        polled.clear();
        return;
    }
    // 옛 노드는 원격 종류를 풀지 못한다.
    if crate::cluster::require_current(cluster).is_err() {
        return;
    }
    // 새 리더는 이전 커밋을 아직 적용하지 않았을 수 있으므로 장벽 뒤에 읽는다.
    if cluster.read_barrier().await.is_err() {
        return;
    }
    let Ok((_, remotes)) = cluster.list::<RemoteSpec>() else {
        return;
    };
    polled.retain(|id, _| remotes.iter().any(|r| r.id == *id));
    let due: Vec<Remote> = remotes
        .into_iter()
        .filter(|r| {
            polled
                .get(&r.id)
                .is_none_or(|t| t.elapsed() >= config.interval)
        })
        .collect();
    let started = Instant::now();
    for remote in &due {
        polled.insert(remote.id, started);
    }
    // 원격들을 동시에 모은다(각각 10초 제한).
    futures_util::future::join_all(due.into_iter().map(|remote| async move {
        let status = collect_remote(&remote).await;
        if let Err(e) = record(cluster, &remote, status).await {
            tracing::warn!(remote = %remote.name, error = %e, "cannot record the remote status");
        }
    }))
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(witness: bool, online: bool, version: Option<&str>) -> RemoteNode {
        RemoteNode {
            spec: RemoteNodeSpec { witness },
            status: RemoteNodeStatus {
                online,
                cpus: 8,
                memory_mib: 16 * 1024 + 512,
                version: version.map(str::to_string),
            },
        }
    }

    fn vm(phase: &str) -> RemoteVm {
        RemoteVm {
            status: RemoteVmStatus {
                phase: phase.into(),
            },
        }
    }

    #[test]
    fn summaries_leave_witnesses_out_and_take_the_lowest_version() {
        let nodes = [
            node(false, true, Some("0.37.0")),
            node(false, false, None),
            node(true, true, Some("0.36.1")),
        ];
        let vms = [vm("running"), vm("stopped"), vm("running")];
        let status = summarize("dr".into(), &nodes, &vms);
        assert_eq!(
            status,
            RemoteStatus {
                reachable: true,
                checked_at: None,
                error: None,
                cluster_name: Some("dr".into()),
                version: Some("0.36.1".into()),
                nodes: Some(NodeCounts {
                    total: 2,
                    online: 1
                }),
                vms: Some(VmCounts {
                    total: 3,
                    running: 2
                }),
                cpus: Some(16),
                memory_gib: Some(33),
            }
        );
        assert_eq!(summarize("dr".into(), &[], &[]).version, None);
    }

    #[test]
    fn remote_lists_ignore_fields_this_build_does_not_know() {
        let nodes: Items<RemoteNode> = serde_json::from_value(serde_json::json!({
            "revision": 9,
            "items": [{ "id": "x", "spec": { "witness": false, "new": 1 },
                        "status": { "online": true, "cpus": 2, "memory_mib": 2048, "later": [] } }]
        }))
        .unwrap();
        assert_eq!(summarize("c".into(), &nodes.items, &[]).cpus, Some(2));
    }

    #[test]
    fn the_token_is_kept_only_for_the_same_url_and_ca() {
        let fp = format!("sha256:{}", "ab".repeat(32));
        let stored = RemoteSpec {
            url: "https://dr:8443".into(),
            ca_fingerprint: fp.clone(),
            token: "opl_0192f0c15d2e70008000000000000000_secret".into(),
            ca_pem: "pem".into(),
        };
        let req = |url: &str, ca: &str, token: Option<&str>| RemoteRequest {
            name: None,
            url: url.into(),
            ca_fingerprint: ca.into(),
            token: token.map(str::to_string),
        };
        // 정규화한 뒤 비교한다. 확인한 CA도 그대로 둔다.
        let same = spec_of(
            &req("https://dr:8443/", &fp.to_uppercase(), None),
            Some(&stored),
        )
        .unwrap();
        assert_eq!(same, stored);
        // 연결 대상이 바뀌면 CA를 비워 다시 받게 한다.
        let new_token = "opl_0192f0c15d2e70008000000000000000_other";
        let moved = spec_of(&req("https://dr2", &fp, Some(new_token)), Some(&stored)).unwrap();
        assert_eq!(moved.ca_pem, "");
        let token_only = spec_of(&req("https://dr:8443", &fp, Some(new_token)), Some(&stored));
        assert_eq!(token_only.unwrap().ca_pem, "pem");
        for changed in [
            req("https://other:8443", &fp, None),
            req(
                "https://dr:8443",
                &format!("sha256:{}", "cd".repeat(32)),
                None,
            ),
        ] {
            let e = spec_of(&changed, Some(&stored)).unwrap_err();
            assert!(
                e.message.contains("token must be given again"),
                "{}",
                e.message
            );
        }
        assert!(spec_of(&req("https://dr", &fp, None), None).is_err());
        let debug = format!("{:?}", req("https://dr", &fp, Some("opl_x_s3cret")));
        assert!(!debug.contains("s3cret"), "{debug}");
    }

    /// 노드 하나짜리 클러스터
    async fn one_node_cluster(dir: &std::path::Path) -> Cluster {
        use opl_cluster::ClusterConfig;
        use opl_cluster::memnet::MemRouter;
        use opl_cluster::types::NodeId;

        let router = MemRouter::default();
        let id = NodeId(Uuid::now_v7());
        let cluster = Cluster::open(ClusterConfig::new(id, dir), router.network(id))
            .await
            .unwrap();
        router.register(id, &cluster);
        cluster
            .initialize(std::collections::BTreeMap::from([(
                id,
                openraft::BasicNode::new(id.to_string()),
            )]))
            .await
            .unwrap();
        cluster
            .raft()
            .wait(Some(Duration::from_secs(10)))
            .metrics(|m| m.state == openraft::ServerState::Leader, "leader")
            .await
            .unwrap();
        cluster
    }

    /// 모으는 사이 다른 쓰기가 있어 적지 못하면 모은 것이 아니라 저장된 것을 돌려준다.
    #[tokio::test]
    async fn a_lost_status_write_returns_the_stored_remote() {
        let dir = tempfile::TempDir::new().unwrap();
        let cluster = one_node_cluster(dir.path()).await;
        let mut remote: Remote = Resource::new(
            "dr",
            RemoteSpec {
                url: "https://dr".into(),
                ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
                token: "opl_0192f0c15d2e70008000000000000000_secret".into(),
                ca_pem: "pem".into(),
            },
        );
        remote.revision = cluster.create(&remote).await.unwrap();
        // 모으는 사이 수집기가 다른 상태를 적었다.
        let meanwhile = RemoteStatus {
            reachable: true,
            checked_at: Some(1),
            cluster_name: Some("dr".into()),
            ..RemoteStatus::default()
        };
        let revision = cluster
            .update_status::<RemoteSpec>(remote.id, &meanwhile)
            .await
            .unwrap();
        let got = recorded(&cluster, remote.clone(), unreachable_status(&"down")).await;
        assert_eq!((got.revision, &got.status), (revision, &meanwhile));

        // 그대로 적을 수 있으면 모은 것을 적는다.
        let got = recorded(&cluster, got, unreachable_status(&"down")).await;
        assert!(got.revision > revision);
        assert_eq!(got.status.error.as_deref(), Some("down"));
        let stored = cluster.get::<RemoteSpec>(remote.id).unwrap().unwrap();
        assert_eq!((stored.revision, stored.status), (got.revision, got.status));
    }
}
