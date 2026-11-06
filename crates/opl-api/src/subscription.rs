//! 구독 `GET/PUT/DELETE /api/v1/subscription`. 키 문자열은 설정(`Settings.spec.subscription`)에
//! 두고, API는 읽을 때 계산한 상태([`SubscriptionInfo`])만 보인다.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::resource::ResourceSpec;
use opl_types::settings::{Settings, SettingsSpec};
use opl_types::subscription::{self, SubscriptionInfo};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::backups::now;
use crate::error::{ApiError, Json};
use crate::settings::{current, write};
use crate::state::AppState;

/// 넣을 구독 키
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionKey {
    /// `opl1.<payload>.<signature>`
    pub key: String,
}

/// 설정에 저장된 키의 지금 상태
fn info(cluster: &Cluster, settings: &Settings) -> Result<SubscriptionInfo, ClusterError> {
    Ok(state_of(&settings.spec, used_nodes(cluster)?))
}

/// 구독이 세는 노드 수: 위트니스가 아닌 노드
pub(crate) fn used_nodes(cluster: &Cluster) -> Result<u32, ClusterError> {
    let used = cluster
        .list::<NodeSpec>()?
        .1
        .iter()
        .filter(|n| !n.spec.witness)
        .count();
    Ok(u32::try_from(used).unwrap_or(u32::MAX))
}

/// 설정 `spec`에 저장된 키의 지금 상태(노드 `used`대)
pub(crate) fn state_of(spec: &SettingsSpec, used: u32) -> SubscriptionInfo {
    subscription::status(
        spec.subscription.as_deref(),
        &subscription::license_keys(),
        now(),
        used,
    )
}

/// 구독 상태. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/subscription", tag = "subscription",
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    let settings = current(&state.cluster)?;
    Ok(Json(info(&state.cluster, &settings)?))
}

/// 구독 키를 넣는다(있던 키는 바꾼다). 형식·서명이 맞지 않거나 만료된 키는 400
/// `invalid_subscription_key`.
#[utoipa::path(put, path = "/subscription", tag = "subscription",
    request_body = SubscriptionKey,
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn put_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SubscriptionKey>,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let key = req.key.trim().to_string();
    let license = subscription::parse(&key, &subscription::license_keys(), now()).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_subscription_key",
            e.to_string(),
        )
    })?;
    crate::cluster::require_current(&state.cluster)?;
    let settings = write(&state.cluster, None, |spec| {
        spec.subscription = Some(key.clone());
        Ok(())
    })
    .await?;
    tracing::info!(id = %license.id, "subscription key set");
    audit.detail(format!("id={}", license.id));
    Ok(Json(info(&state.cluster, &settings)?))
}

/// 구독 키를 지운다. 없으면 그대로 200이다.
#[utoipa::path(delete, path = "/subscription", tag = "subscription",
    responses((status = 200, body = SubscriptionInfo), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_subscription(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<SubscriptionInfo>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    crate::cluster::require_current(&state.cluster)?;
    let settings = write(&state.cluster, None, |spec| {
        spec.subscription = None;
        Ok(())
    })
    .await?;
    tracing::info!("subscription key removed");
    Ok(Json(info(&state.cluster, &settings)?))
}
