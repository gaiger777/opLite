//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.
//! 구독 키 문자열은 API로 내보내지 않는다([`public`]).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{
    Channel, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus, TelemetrySettings,
};
use opl_types::subscription::SubscriptionState;
use opl_types::version::TELEMETRY_URL;
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

/// API로 내보내는 설정: 구독 키 문자열을 뺀다.
pub(crate) fn public(mut settings: Settings) -> Settings {
    settings.spec.subscription = None;
    settings
}

/// 설정에서 바꿀 값. 주어진 필드만 바꾼다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub updates: Option<UpdateSettingsPatch>,
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub telemetry: Option<TelemetryPatch>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub channel: Option<Channel>,
}

/// 텔레메트리에서 바꿀 값
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TelemetryPatch {
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
    /// 보낼 `https://` 주소. `""`는 지워 빌드 기본 끝점을 쓴다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub endpoint: Option<String>,
}

/// 텔레메트리 끝점은 호스트가 있는 `https://` 주소여야 한다.
fn check_endpoint(endpoint: &str) -> Result<(), ApiError> {
    // 설정은 로그인한 누구나 읽으므로 끝점에 사용자 정보를 두지 않는다.
    let authority = endpoint.split_once("://").map_or("", |(_, rest)| {
        rest.split(['/', '?', '#']).next().unwrap_or("")
    });
    if authority.contains('@') {
        return Err(ApiError::bad_request(
            "telemetry.endpoint must not contain credentials",
        ));
    }
    let ok = endpoint.starts_with("https://")
        && endpoint.parse::<Uri>().is_ok_and(|uri| {
            uri.scheme_str() == Some("https") && uri.host().is_some_and(|h| !h.is_empty())
        });
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "telemetry.endpoint must be an https:// URL",
        ))
    }
}

/// 텔레메트리 패치를 `telemetry`에 적용한다. 켜져 있는데 끝점도 빌드 기본 끝점도 없으면 400.
fn apply_telemetry(
    telemetry: &mut TelemetrySettings,
    patch: &TelemetryPatch,
) -> Result<(), ApiError> {
    if let Some(enabled) = patch.enabled {
        telemetry.enabled = enabled;
    }
    match patch.endpoint.as_deref() {
        Some("") => telemetry.endpoint = None,
        Some(endpoint) => telemetry.endpoint = Some(endpoint.to_string()),
        None => {}
    }
    if telemetry.enabled && telemetry.endpoint.is_none() && TELEMETRY_URL.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "telemetry_endpoint_required",
            "set telemetry.endpoint; this build has no default endpoint",
        ));
    }
    Ok(())
}

/// 클러스터 설정. 로그인한 누구나 읽는다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    Ok(Json(public(current(&state.cluster)?)))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다. enterprise 채널은 모든 노드가 이 빌드 이상이고 구독이 active일 때만 고른다
/// (이미 enterprise여도 다시 보내면 검사한다). 텔레메트리도 모든 노드가 이 빌드 이상일 때만 바꾸고,
/// 켤 때 id가 없으면 만든다.
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
    if req.updates.is_none() && req.telemetry.is_none() {
        return Err(ApiError::bad_request("give updates or telemetry"));
    }
    if let Some(endpoint) = req.telemetry.as_ref().and_then(|t| t.endpoint.as_deref())
        && !endpoint.is_empty()
    {
        check_endpoint(endpoint)?;
    }
    crate::cluster::require_upgraded(&state.cluster)?;
    let channel = req.updates.and_then(|u| u.channel);
    let enterprise = channel == Some(Channel::Enterprise);
    // 옛 노드는 enterprise 채널과 텔레메트리 필드를 풀지 못한다.
    if enterprise || req.telemetry.is_some() {
        crate::cluster::require_current(&state.cluster)?;
    }
    let used = if enterprise {
        crate::subscription::used_nodes(&state.cluster)?
    } else {
        0
    };
    let expected = if_match(&headers)?;
    let written = write(&state.cluster, expected, |spec| {
        if enterprise
            && crate::subscription::state_of(spec, used).state != SubscriptionState::Active
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "subscription_required",
                "an active subscription is required for the enterprise channel",
            ));
        }
        if let Some(channel) = channel {
            spec.updates.channel = channel;
        }
        if let Some(telemetry) = &req.telemetry {
            apply_telemetry(&mut spec.telemetry, telemetry)?;
        }
        Ok(())
    })
    .await?;
    let has_id = written
        .status
        .telemetry
        .as_ref()
        .is_some_and(|t| t.id.is_some());
    if !written.spec.telemetry.enabled || has_id {
        return Ok(Json(public(written)));
    }
    // 처음 켰다: 보고에 붙일 id를 만든다(다른 요청이 먼저 만들었으면 그대로).
    let written = write_status(&state.cluster, |status| {
        let telemetry = status.telemetry.get_or_insert_default();
        if telemetry.id.is_none() {
            telemetry.id = Some(Uuid::new_v4());
        }
    })
    .await?;
    Ok(Json(public(written)))
}

/// 텔레메트리 id를 새로 만든다. 설정이 아직 없으면 만든다.
#[utoipa::path(post, path = "/telemetry/reset-id", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn reset_telemetry_id(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    crate::cluster::require_current(&state.cluster)?;
    let written = write_status(&state.cluster, |status| {
        status.telemetry.get_or_insert_default().id = Some(Uuid::new_v4());
    })
    .await?;
    tracing::info!("telemetry id reset");
    Ok(Json(public(written)))
}

/// 설정 status를 읽어 `change`로 바꿔 쓴다. 설정이 아직 없으면 기본 spec으로 만든다. 읽은 뒤 다른
/// 쪽이 썼으면(revision) 다시 읽어 바꾼다. 같은 값이면 쓰지 않는다. 돌려주는 설정은 구독 키를 담고 있다.
pub(crate) async fn write_status(
    cluster: &Cluster,
    change: impl Fn(&mut SettingsStatus),
) -> Result<Settings, ApiError> {
    for _ in 0..10 {
        let settings = current(cluster)?;
        let mut status = settings.status.clone();
        change(&mut status);
        if status == settings.status {
            return Ok(settings);
        }
        let written = if settings.revision == 0 {
            let mut created = Resource::new(SETTINGS_NAME, settings.spec);
            created.status = status;
            match cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(created);
                }
                Err(e) => e,
            }
        } else {
            match cluster
                .update_status_checked::<SettingsSpec>(settings.id, settings.revision, &status)
                .await
            {
                Ok(_) => return Ok(current(cluster)?),
                Err(e) => e,
            }
        };
        match written {
            ClusterError::Rejected(Rejection::NameTaken { .. })
            | ClusterError::Rejected(Rejection::RevisionMismatch { .. }) => continue,
            e => return Err(e.into()),
        }
    }
    Err(ApiError::conflict(
        "busy",
        "the settings kept changing while being updated; try again",
    ))
}

/// 설정을 읽어 `change`로 바꿔 쓴다. 설정이 아직 없으면 만든다(`expected`는 0). `expected`가
/// 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지 않는다.
/// 돌려주는 설정은 구독 키를 담고 있다.
pub(crate) async fn write(
    cluster: &Cluster,
    expected: Option<u64>,
    change: impl Fn(&mut SettingsSpec) -> Result<(), ApiError>,
) -> Result<Settings, ApiError> {
    for _ in 0..10 {
        let settings = current(cluster)?;
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
        change(&mut spec)?;
        if spec == settings.spec {
            return Ok(settings);
        }
        let written = if settings.revision == 0 {
            // 아직 없다. 다른 요청이 먼저 만들었으면 다시 읽는다.
            let mut created = Resource::new(SETTINGS_NAME, spec);
            match cluster.create(&created).await {
                Ok(revision) => {
                    created.revision = revision;
                    return Ok(created);
                }
                Err(e) => e,
            }
        } else {
            match cluster
                .update_spec(
                    settings.id,
                    Some(settings.revision),
                    settings.labels.clone(),
                    &spec,
                )
                .await
            {
                Ok(_) => return Ok(current(cluster)?),
                Err(e) => e,
            }
        };
        // If-Match 0으로 만들려 했는데 다른 요청이 먼저 만들었다.
        if let (ClusterError::Rejected(Rejection::NameTaken { .. }), Some(expected)) =
            (&written, expected)
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: current(cluster)?.revision,
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
