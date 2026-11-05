//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.

use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::resources::if_match;
use crate::state::AppState;

/// 저장된 설정. 없으면 기본값을 담은 리소스(id는 nil UUID, revision 0)다.
pub fn current(cluster: &Cluster) -> Result<Settings, ClusterError> {
    Ok(cluster
        .get_by_name::<SettingsSpec>(SETTINGS_NAME)?
        .unwrap_or_else(|| Resource {
            id: Uuid::nil(),
            ..Resource::new(SETTINGS_NAME, SettingsSpec::default())
        }))
}

/// 설정에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub updates: Option<UpdateSettingsPatch>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub channel: Option<Channel>,
}

/// 클러스터 설정. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    Ok(Json(current(&state.cluster)?))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
    params(("If-Match" = Option<u64>, Header)),
    request_body = SettingsPatch,
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    headers: HeaderMap,
    Json(req): Json<SettingsPatch>,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    let Some(updates) = req.updates else {
        return Err(ApiError::bad_request("give updates"));
    };
    crate::cluster::require_upgraded(&state.cluster)?;
    let expected = if_match(&headers)?;
    for _ in 0..10 {
        let settings = current(&state.cluster)?;
        if let Some(expected) = expected
            && expected != settings.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: settings.revision,
            })
            .into());
        }
        let mut spec = settings.spec.clone();
        if let Some(channel) = updates.channel {
            spec.updates.channel = channel;
        }
        if spec == settings.spec {
            return Ok(Json(settings));
        }
        let written = if settings.revision == 0 {
            // 아직 없다. 다른 요청이 먼저 만들었으면 다시 읽는다.
            let mut created = Resource::new(SETTINGS_NAME, spec);
            match state.cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(Json(created));
                }
                Err(e) => e,
            }
        } else {
            match state
                .cluster
                .update_spec(
                    settings.id,
                    Some(settings.revision),
                    settings.labels.clone(),
                    &spec,
                )
                .await
            {
                Ok(_) => return Ok(Json(current(&state.cluster)?)),
                Err(e) => e,
            }
        };
        // If-Match 0으로 만들려 했는데 다른 요청이 먼저 만들었다.
        if let (ClusterError::Rejected(Rejection::NameTaken { .. }), Some(expected)) =
            (&written, expected)
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current(&state.cluster)?.revision,
            })
            .into());
        }
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. })
                if expected.is_none() =>
            {
                continue;
            }
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}
