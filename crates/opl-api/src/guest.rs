//! 게스트 에이전트 API(계획 59 §8·§9): 게스트 정보 읽기와 게스트 사용자 비밀번호 설정. VM이 이 노드에서 돌면 이 노드의
//! 게스트 정보 캐시([`AppState::guest`])에, 다른 노드에서 돌면 노드 사이 RPC `GuestAgent`로 묻는다. 비밀번호는 저장하지도
//! 로그·감사·오류 문구에 넣지도 않는다.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use opl_cluster::types::NodeId;
use opl_peer::{GuestAgentError, GuestAgentOp, GuestAgentRequest};
use opl_types::guest::{GuestInfoResponse, GuestInfoState, validate_guest_password};
use opl_types::node::{GUEST_AGENT, NodeSpec};
use opl_types::vm::{Vm, VmPhase, VmSpec};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::lookup;
use crate::state::AppState;

/// 다른 노드의 캐시를 읽는 한도(지표 읽기와 같다)
const GUEST_READ_TIMEOUT: Duration = Duration::from_secs(3);
/// `refresh`로 지금 묻는 읽기의 한도. 에이전트에 명령 다섯을 묻는 데 최대 20초쯤 걸린다.
const GUEST_REFRESH_TIMEOUT: Duration = Duration::from_secs(25);
/// 비밀번호 설정의 한도(에이전트 동기화 2초 + 명령 5초, 앞 요청을 기다리는 시간)
const GUEST_PASSWORD_TIMEOUT: Duration = Duration::from_secs(15);

/// 게스트 정보 읽기의 조건
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct GuestParams {
    /// 캐시(60초마다 모음) 대신 에이전트에 지금 묻는다. VM마다 5초에 한 번이고 그 사이에는 캐시를 돌려준다.
    #[serde(default)]
    pub refresh: bool,
}

/// 게스트 사용자 비밀번호 설정
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GuestPasswordRequest {
    /// 게스트 사용자 이름(1~64바이트, 제어 문자 없음)
    pub user: String,
    /// 새 비밀번호(평문, 1~256바이트). 저장하지 않는다.
    #[schema(format = Password)]
    pub password: String,
}

/// 비밀번호는 적지 않는다.
impl fmt::Debug for GuestPasswordRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestPasswordRequest")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// 비밀번호를 설정한 사용자
#[derive(Debug, Serialize, ToSchema)]
pub struct GuestPasswordResponse {
    pub user: String,
}

/// VM이 도는 노드(실행 중이 아니면 None)
fn running_node(vm: &Vm) -> Option<uuid::Uuid> {
    vm.status
        .node
        .filter(|_| matches!(vm.status.phase, VmPhase::Running | VmPhase::Stopping))
}

/// VM에 게스트 에이전트 채널이 있는지: 스펙이나 도는 QEMU(실행 중에 끈 채널은 다시 켤 때까지 남는다)
fn has_channel(vm: &Vm) -> bool {
    vm.spec.agent_channel() || vm.status.hardware.as_ref().is_some_and(|hw| hw.guest_agent)
}

/// VM이 도는 노드 `node`의 게스트 에이전트에 `op`를 묻는다: 이 노드면 [`AppState::guest`], 아니면 RPC `GuestAgent`.
/// 정보 읽기면 그 답이다.
async fn ask(
    state: &AppState,
    vm: &Vm,
    node: uuid::Uuid,
    op: GuestAgentOp,
    timeout: Duration,
) -> Result<Option<GuestInfoResponse>, GuestAgentError> {
    if node == state.node {
        let Some(source) = &state.guest else {
            return Err(GuestAgentError::Unsupported);
        };
        return match op {
            GuestAgentOp::Info { refresh } => Ok(Some(source.info(vm.id, refresh).await)),
            GuestAgentOp::SetPassword { user, password } => source
                .set_password(vm.id, &user, &password)
                .await
                .map(|()| None),
        };
    }
    if state
        .cluster
        .get::<NodeSpec>(node)
        .ok()
        .flatten()
        .is_some_and(|n| !n.status.online)
    {
        return Err(GuestAgentError::Unreachable("the node is offline".into()));
    }
    let Some(peers) = &state.peers else {
        return Err(GuestAgentError::Unreachable(
            "no node-to-node connection".into(),
        ));
    };
    let Some(addr) = state
        .cluster
        .members()
        .addr(NodeId(node))
        .map(str::to_string)
    else {
        return Err(GuestAgentError::Unreachable("not a cluster member".into()));
    };
    let req = GuestAgentRequest { vm: vm.id, op };
    let reply = tokio::time::timeout(
        timeout,
        peers.guest_agent(NodeId(node), &addr, req, timeout),
    )
    .await
    .map_err(|_| GuestAgentError::Unreachable("timed out".into()))??;
    match reply.error {
        Some(e) => Err(e),
        None => Ok(reply.info),
    }
}

/// 켜진 VM의 게스트 정보(캐시 — 지금 묻지 않음). 꺼졌거나 채널이 없거나 답하지 않으면 None(계획 67 §9.2 옮기기 계획의
/// 주소 출처 `guest-agent`).
pub(crate) async fn cached_info(state: &AppState, vm: &Vm) -> Option<opl_types::guest::GuestInfo> {
    let node = running_node(vm)?;
    if !has_channel(vm) {
        return None;
    }
    let op = GuestAgentOp::Info { refresh: false };
    match Box::pin(ask(state, vm, node, op, GUEST_READ_TIMEOUT)).await {
        Ok(Some(response)) => response.info,
        _ => None,
    }
}

/// 답하지 못한 이유를 게스트 정보 상태로 바꾼다.
fn info_state(e: &GuestAgentError) -> GuestInfoState {
    match e {
        GuestAgentError::NotRunning => GuestInfoState::NotRunning,
        GuestAgentError::NoAgent => GuestInfoState::NoAgent,
        GuestAgentError::Unsupported => GuestInfoState::Unsupported,
        GuestAgentError::Unavailable(_)
        | GuestAgentError::Invalid(_)
        | GuestAgentError::Unreachable(_) => GuestInfoState::Unreachable,
    }
}

/// VM의 게스트 정보(OS·호스트 이름·NIC와 IP·파일 시스템). VM이 도는 노드의 메모리 캐시(60초마다 모음)를 읽고,
/// `refresh`면 에이전트에 지금 묻는다(VM마다 5초에 한 번). `state`는 `answered`(정보가 있음), `no_agent`(채널이 없거나
/// 에이전트가 아직 답하지 않음), `not_running`, `unreachable`(VM이 도는 노드가 답하지 않음), `unsupported`(그 노드가
/// 게스트 에이전트를 모르는 옛 노드)다. 정보는 `answered`일 때만 있다.
#[utoipa::path(get, path = "/vms/{key}/guest", tag = "vms",
    params(("key" = String, Path, description = "id or name"), GuestParams),
    responses((status = 200, body = GuestInfoResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_guest(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    Path(key): Path<String>,
    Query(params): Query<GuestParams>,
) -> Result<Json<GuestInfoResponse>, ApiError> {
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    let only = |state| GuestInfoResponse { state, info: None };
    let Some(node) = running_node(&vm) else {
        return Ok(Json(only(GuestInfoState::NotRunning)));
    };
    if !has_channel(&vm) {
        return Ok(Json(only(GuestInfoState::NoAgent)));
    }
    let timeout = if params.refresh {
        GUEST_REFRESH_TIMEOUT
    } else {
        GUEST_READ_TIMEOUT
    };
    let op = GuestAgentOp::Info {
        refresh: params.refresh,
    };
    let response = match ask(&state, &vm, node, op, timeout).await {
        Ok(Some(response)) => response,
        Ok(None) => only(GuestInfoState::Unsupported),
        Err(e) => {
            tracing::debug!(vm = %vm.id, error = %e, "cannot read the guest information");
            only(info_state(&e))
        }
    };
    Ok(Json(response))
}

/// 비밀번호 설정이 실패한 이유를 API 오류로 바꾼다. 문구에 비밀번호가 들지 않는다.
fn password_error(vm: &Vm, e: GuestAgentError) -> ApiError {
    match e {
        GuestAgentError::NotRunning => not_running(vm),
        GuestAgentError::NoAgent => no_channel(vm),
        GuestAgentError::Unavailable(why) => ApiError::conflict(
            "guest_agent_unavailable",
            format!("the guest agent of VM {:?} did not answer: {why}", vm.name),
        ),
        GuestAgentError::Invalid(why) => ApiError::bad_request(why),
        GuestAgentError::Unsupported => ApiError::conflict(
            "cluster_upgrading",
            format!(
                "the node running VM {:?} does not support the guest agent yet; upgrade it first",
                vm.name
            ),
        ),
        GuestAgentError::Unreachable(why) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "node_unreachable",
            format!("the node running VM {:?} did not answer: {why}", vm.name),
        ),
    }
}

fn not_running(vm: &Vm) -> ApiError {
    ApiError::conflict("vm_not_running", format!("VM {:?} is not running", vm.name))
}

fn no_channel(vm: &Vm) -> ApiError {
    ApiError::conflict(
        "guest_agent_unavailable",
        format!(
            "VM {:?} has no guest agent channel; turn on guest_agent and restart the VM",
            vm.name
        ),
    )
}

/// 게스트 사용자 `user`의 비밀번호를 바꾼다(`guest-set-user-password`, 설계 §9). 게스트 안 관리자 권한과 같은 무게라
/// VM 쓰기 권한이 필요하다. 비밀번호는 저장하지 않고 로그·감사(`vm guest password win user X`)·오류에 넣지 않는다.
/// 사용자 이름(1~64바이트, 제어 문자 없음)·비밀번호(1~256바이트)가 맞지 않으면 400, 모든 노드가 `guest-agent`를 알아야
/// 하고(409 `cluster_upgrading`), VM이 돌지 않으면 409 `vm_not_running`, 채널이 없거나 에이전트가 답하지 않으면 409
/// `guest_agent_unavailable`, VM이 도는 노드가 답하지 않으면 503 `node_unreachable`이다.
#[utoipa::path(post, path = "/vms/{key}/guest/password", tag = "vms",
    params(("key" = String, Path, description = "id or name")),
    request_body = GuestPasswordRequest,
    responses((status = 200, body = GuestPasswordResponse), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn set_guest_password(
    State(state): State<Arc<AppState>>,
    _principal: Principal,
    audit: AuditNote,
    Path(key): Path<String>,
    Json(req): Json<GuestPasswordRequest>,
) -> Result<Json<GuestPasswordResponse>, ApiError> {
    require_feature(&state.cluster, GUEST_AGENT)?;
    let GuestPasswordRequest { user, password } = req;
    validate_guest_password(&user, &password).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let vm = lookup::<VmSpec>(&state.cluster, &key)?;
    audit.detail(format!("vm guest password {} user {user}", vm.name));
    let Some(node) = running_node(&vm) else {
        return Err(not_running(&vm));
    };
    if !has_channel(&vm) {
        return Err(no_channel(&vm));
    }
    let op = GuestAgentOp::SetPassword {
        user: user.clone(),
        password,
    };
    ask(&state, &vm, node, op, GUEST_PASSWORD_TIMEOUT)
        .await
        .map_err(|e| password_error(&vm, e))?;
    Ok(Json(GuestPasswordResponse { user }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_become_states_or_api_errors() {
        assert_eq!(
            info_state(&GuestAgentError::Unsupported),
            GuestInfoState::Unsupported
        );
        assert_eq!(
            info_state(&GuestAgentError::Unreachable("timed out".into())),
            GuestInfoState::Unreachable
        );
        assert_eq!(
            info_state(&GuestAgentError::NoAgent),
            GuestInfoState::NoAgent
        );
        let spec: VmSpec = serde_json::from_value(serde_json::json!({
            "cpu": { "cores": 1 }, "memory_mib": 256, "machine": "virt", "nics": [],
        }))
        .unwrap();
        let vm: Vm = opl_types::resource::Resource::new("win", spec);
        let e = password_error(&vm, GuestAgentError::Unavailable("timed out".into()));
        assert_eq!(
            (e.status, e.code),
            (StatusCode::CONFLICT, "guest_agent_unavailable")
        );
        let e = password_error(&vm, GuestAgentError::Invalid("bad user".into()));
        assert_eq!(e.status, StatusCode::BAD_REQUEST);
        let e = password_error(&vm, GuestAgentError::Unreachable("offline".into()));
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn the_password_never_shows_in_debug_output() {
        let req = GuestPasswordRequest {
            user: "Administrator".into(),
            password: "hunter2".into(),
        };
        let shown = format!("{req:?}");
        assert!(
            shown.contains("Administrator") && !shown.contains("hunter2"),
            "{shown}"
        );
    }
}
