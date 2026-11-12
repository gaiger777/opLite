//! 헬스 체크(`GET /health`, 설계 §4). 로드 밸런서·keepalived·쿠버네티스가 인증 없이 부른다.
//!
//! 기본(`?ready`)은 이 노드가 API 요청을 받아도 되는지 본다: 리더를 알고(리더면 과반수가 답함), 적용이
//! 로그보다 [`MAX_APPLY_LAG`] 넘게 뒤처지지 않고, HA 펜싱을 쓰면 리스가 fresh이고, 마지막 실행기 확인에서
//! `core`·`api`가 늦지 않았고, 유지보수 중이 아니고, `api` 역할이다. `?live`는 프로세스가 답하는지만 본다.
//! 로그인하지 않은 요청에는 `status`·`failed`만 보인다(노드 id·리더 같은 내부 정보를 감춘다).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use opl_types::node::NodeSpec;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::Principal;
use crate::error::{ApiError, Json, Query};
use crate::state::AppState;

/// 이 노드의 적용이 이보다 오래 로그를 따라잡지 못하면 준비되지 않았다.
pub const MAX_APPLY_LAG: Duration = Duration::from_secs(5);

/// 마지막 실행기 확인 회차에 늦은 실행기(`core`·`api`·`work`)를 알려 준다(opld의 `Liveness`).
pub trait RuntimeProbe: Send + Sync {
    fn slow(&self) -> Vec<&'static str>;
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct HealthQuery {
    /// 있으면(값은 보지 않는다) 프로세스가 답하는지만 본다(언제나 200).
    #[serde(default)]
    pub live: Option<String>,
    /// 있으면(값은 보지 않는다) 준비됐는지 본다. 기본이다.
    #[serde(default)]
    pub ready: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthStatus {
    Ok,
    Unavailable,
}

/// 이 노드의 HA 리스
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaseHealth {
    Fresh,
    /// 펜싱을 쓰는데 리스가 fresh가 아니다(갱신하지 못함).
    Expired,
    /// HA 펜싱을 쓰지 않는다.
    None,
}

/// 마지막 실행기 확인에서 `core`·`api`가 제때 답했는지(`work`만 늦으면 `ok`)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHealth {
    Ok,
    Slow,
}

/// 실패할 수 있는 확인 하나
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheck {
    /// 리더를 모르거나, 리더인데 과반수가 답하지 않는다.
    Quorum,
    /// 적용이 로그보다 5초 넘게 뒤처졌다.
    ApplyLag,
    /// HA 펜싱을 쓰는데 리스가 fresh가 아니다.
    Lease,
    /// 마지막 실행기 확인에서 `core`나 `api`가 늦었다.
    Runtimes,
    /// 노드가 유지보수(비우기) 중이다.
    Maintenance,
    /// 노드가 `api` 역할이 아니다(Node 리소스가 아직 없는 경우 포함).
    Role,
}

/// 헬스 체크 결과. 200과 503이 같은 모양이다. `status`·`failed` 밖의 필드는 로그인(세션·토큰)한 요청에만
/// 있다.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Health {
    pub status: HealthStatus,
    /// 이 노드의 id
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<Uuid>,
    /// 이 노드가 아는 리더(모르면 없다)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<Uuid>,
    /// 리더를 알고, 이 노드가 리더면 과반수가 최근 답했다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum: Option<bool>,
    /// 이 노드 로그의 엔트리를 적용하지 못한 채 지난 시간(밀리초)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_lag_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<LeaseHealth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtimes: Option<RuntimeHealth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance: Option<bool>,
    /// `api` 역할 노드인지
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<bool>,
    /// 실패한 확인. `?live`면 언제나 비어 있다.
    pub failed: Vec<HealthCheck>,
}

impl Health {
    /// 로그인하지 않은 요청에 보이는 것(`status`·`failed`)만 남긴다.
    fn brief(self) -> Self {
        Self {
            status: self.status,
            node: None,
            leader: None,
            quorum: None,
            apply_lag_ms: None,
            lease: None,
            runtimes: None,
            maintenance: None,
            api: None,
            failed: self.failed,
        }
    }
}

/// 이 노드의 헬스. 인증 없이 읽는다. `?live`는 언제나 200, 기본(`?ready`)은 실패한 확인이 있으면 503이다.
/// 로그인하지 않았으면 `status`·`failed`만 답한다.
#[utoipa::path(get, path = "/health", tag = "cluster", security(()),
    params(HealthQuery),
    responses((status = 200, body = Health), (status = 503, body = Health)))]
pub async fn get_health(
    State(state): State<Arc<AppState>>,
    principal: Result<Principal, ApiError>,
    Query(q): Query<HealthQuery>,
) -> Result<(StatusCode, Json<Health>), ApiError> {
    let live = q.live.is_some() && q.ready.is_none();
    let mut health = check(&state, live)?;
    if principal.is_err() {
        health = health.brief();
    }
    let code = match health.status {
        HealthStatus::Ok => StatusCode::OK,
        HealthStatus::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    };
    Ok((code, Json(health)))
}

/// 이 노드의 상태를 모으고, `live`가 아니면 준비 조건을 확인한다.
pub fn check(state: &AppState, live: bool) -> Result<Health, ApiError> {
    let raft = state.cluster.health();
    // `?live`는 저장소를 읽지 못해도 200이다(프로세스가 답하는지만 본다).
    let node = match state.cluster.get::<NodeSpec>(state.node) {
        Err(_) if live => None,
        node => node?,
    };
    let lease = match &state.ha {
        Some(ha) if ha.fresh() => LeaseHealth::Fresh,
        Some(_) => LeaseHealth::Expired,
        None => LeaseHealth::None,
    };
    let slow = state
        .runtimes
        .as_ref()
        .is_some_and(|r| r.slow().iter().any(|name| matches!(*name, "core" | "api")));
    let maintenance = node.as_ref().is_some_and(|n| n.spec.maintenance);
    let api = node.as_ref().is_some_and(|n| n.spec.is_api());
    let mut failed = Vec::new();
    if !live {
        let checks = [
            (!raft.quorum, HealthCheck::Quorum),
            (raft.apply_lag > MAX_APPLY_LAG, HealthCheck::ApplyLag),
            (lease == LeaseHealth::Expired, HealthCheck::Lease),
            (slow, HealthCheck::Runtimes),
            (maintenance, HealthCheck::Maintenance),
            (!api, HealthCheck::Role),
        ];
        failed = checks
            .into_iter()
            .filter_map(|(bad, check)| bad.then_some(check))
            .collect();
    }
    Ok(Health {
        status: if failed.is_empty() {
            HealthStatus::Ok
        } else {
            HealthStatus::Unavailable
        },
        node: Some(state.node),
        leader: raft.leader.map(|id| id.0),
        quorum: Some(raft.quorum),
        apply_lag_ms: Some(u64::try_from(raft.apply_lag.as_millis()).unwrap_or(u64::MAX)),
        lease: Some(lease),
        runtimes: Some(if slow {
            RuntimeHealth::Slow
        } else {
            RuntimeHealth::Ok
        }),
        maintenance: Some(maintenance),
        api: Some(api),
        failed,
    })
}
