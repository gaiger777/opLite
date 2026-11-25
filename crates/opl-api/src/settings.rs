//! 클러스터 설정 `GET/PATCH /api/v1/settings`. 리소스가 없으면 기본값으로 보고 처음 PATCH가 만든다.
//! 구독 키 문자열은 API로 내보내지 않는다([`public`]).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::history::HistorySettings;
use opl_types::node::{CPU_MODELS, FAST_HA, HA_PLACEMENT, METRICS_HISTORY, PLACEMENT, TWO_FACTOR};
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::settings::{
    Channel, DEFAULT_HA_START_CONCURRENCY, HaReserve, MAX_HA_RESERVE_NODES, MAX_HA_RESERVE_PERCENT,
    MAX_HA_START_CONCURRENCY, MAX_PLACEMENT_THRESHOLD, PlacementMode, PlacementPolicy,
    PlacementSettings, PrometheusSettings, SETTINGS_NAME, Settings, SettingsSpec, SettingsStatus,
    ShutdownPolicy, TelemetrySettings, VOTER_LIMITS,
};
use opl_types::subscription::SubscriptionState;
use opl_types::version::TELEMETRY_URL;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit::AuditNote;
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
    /// 쿠키 요청을 받는 다른 출처(`https://portal.example.com`) 전체. `[]`는 지워 같은 출처만 받는다.
    /// `https://`만 받는다(개발용 `http://localhost`·`127.0.0.1`·`[::1]`은 예외).
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub web_origins: Option<Vec<String>>,
    /// 노드 API 인증서에 더할 이름(DNS 이름·IP) 전체. `[]`는 지워 노드 이름만 쓴다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub api_names: Option<Vec<String>>,
    /// 투표 멤버 상한: 3, 5, 7 가운데 하나(위트니스도 센다). 리더가 learner를 올리거나 투표 멤버를 learner로 내려
    /// 맞춘다.
    #[serde(default)]
    #[schema(nullable = false, default, minimum = 3, maximum = 7)]
    pub voters: Option<u8>,
    /// 호스트가 꺼지는 중(재부팅·전원 끄기)일 때 노드가 HA VM을 다루는 방법. `conditional`(기본: 재부팅이면
    /// `freeze`, 끄기면 `failover`), `freeze`, `failover`, `migrate`. `systemctl stop`에는 쓰지 않는다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub shutdown_policy: Option<ShutdownPolicy>,
    /// HA 복구를 위해 비워 둘 용량: `{"nodes": N}`(메모리가 가장 큰 노드 N대, 1~8), `{"percent": P}`(1~90),
    /// `"none"`(예약 없음). HA VM을 켤 때 넘으면 409 `ha_capacity`다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub ha_reserve: Option<HaReservePatch>,
    /// HA 복구 때 대상 노드 하나에 한꺼번에 옮겨 켜는 VM 수(1~32, 기본 4)
    #[serde(default)]
    #[schema(nullable = false, default, minimum = 1, maximum = 32)]
    pub ha_start_concurrency: Option<u8>,
    /// 최적 배치: 점수 정책, 균형 조정기의 동작, 불균형 문턱. 준 값만 바꾼다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub placement: Option<PlacementPatch>,
    /// Prometheus 끝점(`GET /api/v1/metrics`). 모든 노드가 `metrics-history`를 알아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub prometheus: Option<PrometheusPatch>,
    /// 2단계 인증 정책(계획 62) 전체: 전역 역할(`roles`)·영역(`realms`, 로컬 사용자는 `local`)에 요구하고, 패스키의
    /// RP ID(`rp_id`)를 정한다. `{}`는 정책을 지운다. OIDC 영역은 400(IdP가 MFA를 맡는다). 모든 노드가
    /// `two-factor`를 알아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub two_factor: Option<opl_types::two_factor::TwoFactorPolicy>,
    /// 게스트 CPU(계획 63): 클러스터 기준 모드·고정 기준·자동 플래그·숨김 목록. 준 칸만 바꾼다. 실행 중 VM은 다음 전원
    /// 껐다 켜기까지 그대로이고 응답의 `affected_running_vms`가 그 수다. 모든 노드가 `cpu-models`를 알아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub cpu: Option<crate::cpu::CpuSettingsPatch>,
    /// ACME(계획 73 §11.3): 포트 80 리스너의 https 리다이렉트·갱신 시각 흩뜨리기. 준 칸만 바꾼다. 모든 노드가 `acme`를
    /// 알아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub acme: Option<AcmeSettingsPatch>,
    /// 설정 이력 보존 한도(계획 73 §7.3): 리소스당 줄 수(10~1000)·나이(일, 7~3650)·전체 크기(MiB, 8~512). 준 칸만 바꾼다.
    /// 모든 노드가 `config-history`를 알아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub history: Option<HistorySettingsPatch>,
}

/// 설정 이력 보존 한도에서 바꿀 값
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HistorySettingsPatch {
    #[serde(default)]
    #[schema(nullable = false, minimum = 10, maximum = 1000)]
    pub max_per_resource: Option<u32>,
    #[serde(default)]
    #[schema(nullable = false, minimum = 7, maximum = 3650)]
    pub max_age_days: Option<u32>,
    #[serde(default)]
    #[schema(nullable = false, minimum = 8, maximum = 512)]
    pub max_total_mib: Option<u32>,
}

impl HistorySettingsPatch {
    /// 준 값만 `current`에 합친다.
    fn apply(&self, mut current: HistorySettings) -> HistorySettings {
        if let Some(n) = self.max_per_resource {
            current.max_per_resource = n;
        }
        if let Some(n) = self.max_age_days {
            current.max_age_days = n;
        }
        if let Some(n) = self.max_total_mib {
            current.max_total_mib = n;
        }
        current
    }
}

/// ACME 동작에서 바꿀 값
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AcmeSettingsPatch {
    #[serde(default)]
    #[schema(nullable = false)]
    pub http_redirect: Option<bool>,
    #[serde(default)]
    #[schema(nullable = false)]
    pub renew_jitter: Option<bool>,
}

/// `PATCH /settings`의 응답: 저장한 설정과, `cpu`를 바꿨으면 다음 전원 껐다 켜기에 CPU가 바뀌는 실행 중 VM 수
#[derive(Debug, Serialize)]
pub struct SettingsPatched {
    #[serde(flatten)]
    pub settings: Settings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub affected_running_vms: Option<u32>,
}

// 제네릭 리소스를 펼친 모양은 derive가 이름을 잘못 잇는다(다른 `Resource`를 가리킴). 손으로 allOf로 적는다.
impl utoipa::PartialSchema for SettingsPatched {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        use utoipa::openapi::schema::{AllOfBuilder, ObjectBuilder, Type};
        let extra = ObjectBuilder::new().property(
            "affected_running_vms",
            ObjectBuilder::new()
                .schema_type(Type::Integer)
                .format(Some(utoipa::openapi::SchemaFormat::KnownFormat(
                    utoipa::openapi::KnownFormat::Int32,
                )))
                .minimum(Some(0))
                .description(Some(
                    "running VMs whose CPU changes at their next power cycle (only when cpu was given)",
                )),
        );
        AllOfBuilder::new()
            .item(<Resource<SettingsSpec, SettingsStatus> as utoipa::PartialSchema>::schema())
            .item(extra)
            .into()
    }
}

impl ToSchema for SettingsPatched {}

/// Prometheus 끝점에서 바꿀 값
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PrometheusPatch {
    /// 켜면 각 노드가 `GET /api/v1/metrics`로 지표를 내보낸다(Node·Vm 읽기 권한 토큰). 끄면 404다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub enabled: Option<bool>,
}

/// 최적 배치에서 바꿀 값
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PlacementPatch {
    /// `balance`(고르게, 기본)나 `pack`(적은 노드에 모음)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub policy: Option<PlacementPolicy>,
    /// 균형 조정기: `off`, `recommend`(추천만, 기본), `auto`(스스로 옮김)
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub mode: Option<PlacementMode>,
    /// 불균형 문턱(백분율 1~50, 기본 15)
    #[serde(default)]
    #[schema(nullable = false, default, minimum = 1, maximum = 50)]
    pub threshold: Option<u8>,
}

impl PlacementPatch {
    /// 준 값만 `current`에 합친다.
    fn apply(&self, mut current: PlacementSettings) -> PlacementSettings {
        if let Some(policy) = self.policy {
            current.policy = policy;
        }
        if let Some(mode) = self.mode {
            current.mode = mode;
        }
        if let Some(threshold) = self.threshold {
            current.threshold = threshold;
        }
        current
    }
}

/// HA 용량 예약 바꾸기: `"none"`이나 예약
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum HaReservePatch {
    Off(HaReserveOff),
    Set(HaReserve),
}

/// 예약을 지운다.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum HaReserveOff {
    None,
}

/// API 이름은 이만큼까지 둔다.
const MAX_API_NAMES: usize = 32;

/// API 이름(DNS 이름이나 IP)을 비교할 모양으로 바꾼다: IP는 표준 표기, 이름은 소문자. 와일드카드·포트·
/// 숫자만 있는 이름은 None.
pub(crate) fn normalize_api_name(name: &str) -> Option<String> {
    if let Ok(ip) = name.parse::<std::net::IpAddr>() {
        return Some(ip.to_string());
    }
    let name = name.to_ascii_lowercase();
    let name = name.strip_suffix('.').unwrap_or(&name);
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    // 숫자로만 된 끝 라벨은 IP로 잘못 쓴 것이다(`10.0.0`).
    let ok = name.len() <= 253
        && name.split('.').all(label_ok)
        && !name.rsplit('.').next()?.bytes().all(|b| b.is_ascii_digit());
    ok.then(|| name.to_string())
}

/// API 이름 목록을 검사하고 비교할 모양으로 바꾼다(겹친 것은 하나로).
fn api_names(names: &[String]) -> Result<Vec<String>, ApiError> {
    if names.len() > MAX_API_NAMES {
        return Err(ApiError::bad_request(format!(
            "api_names can have at most {MAX_API_NAMES} names"
        )));
    }
    let mut out = Vec::new();
    for name in names {
        let normalized = normalize_api_name(name).ok_or_else(|| {
            ApiError::bad_request(format!(
                "api_names: {name:?} is not a DNS name or an IP address"
            ))
        })?;
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

/// 허용 출처는 이만큼까지 둔다.
const MAX_WEB_ORIGINS: usize = 32;

/// 출처(`scheme://host[:port]`)를 비교할 모양으로 바꾼다: 소문자로, 기본 포트(443·80)는 뺀다(브라우저의
/// `Origin`처럼). http(s)가 아니거나 경로·사용자 정보가 있으면 None.
pub(crate) fn normalize_origin(origin: &str) -> Option<String> {
    let lower = origin.to_ascii_lowercase();
    let (scheme, authority) = lower.split_once("://")?;
    let default_port = match scheme {
        "https" => ":443",
        "http" => ":80",
        _ => return None,
    };
    if authority.contains(['/', '?', '#', '@']) {
        return None;
    }
    let parsed: axum::http::uri::Authority = authority.parse().ok()?;
    if parsed.host().is_empty() {
        return None;
    }
    let authority = authority.strip_suffix(default_port).unwrap_or(authority);
    Some(format!("{scheme}://{authority}"))
}

/// 허용 출처로 둘 수 있다: `https://`, 또는 개발용 `http://localhost`·`127.0.0.1`·`[::1]`(포트는 아무것).
/// `normalized`는 [`normalize_origin`]의 결과다.
fn secure_origin(normalized: &str) -> bool {
    if normalized.starts_with("https://") {
        return true;
    }
    normalized
        .strip_prefix("http://")
        .and_then(|a| a.parse::<axum::http::uri::Authority>().ok())
        .is_some_and(|a| matches!(a.host(), "localhost" | "127.0.0.1" | "[::1]"))
}

/// 허용 출처 목록을 검사하고 비교할 모양으로 바꾼다(겹친 것은 하나로). 쿠키를 `SameSite=None; Secure`로
/// 주므로 `https://`만 받는다(개발용 루프백 `http://`는 예외).
fn web_origins(origins: &[String]) -> Result<Vec<String>, ApiError> {
    if origins.len() > MAX_WEB_ORIGINS {
        return Err(ApiError::bad_request(format!(
            "web_origins can have at most {MAX_WEB_ORIGINS} origins"
        )));
    }
    let mut out = Vec::new();
    for origin in origins {
        let normalized = normalize_origin(origin).ok_or_else(|| {
            ApiError::bad_request(format!(
                "web_origins: {origin:?} is not an origin like https://portal.example.com"
            ))
        })?;
        if !secure_origin(&normalized) {
            return Err(ApiError::bad_request(format!(
                "web_origins: {origin:?} must use https:// (http:// only for localhost)"
            )));
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
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

/// 클러스터 설정. 로그인한 누구나 읽는다. 관리자(설정 쓰기 권한)가 아니면 이미지 카탈로그의 URL을 가린다.
#[utoipa::path(get, path = "/settings", tag = "settings",
    responses((status = 200, body = Resource<SettingsSpec, SettingsStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<Settings>, ApiError> {
    state.authorize(&principal, Action::Read, SettingsSpec::KIND)?;
    let mut settings = public(current(&state.cluster)?);
    if !crate::images::writes_settings(&state, &principal) {
        settings
            .spec
            .image_catalog
            .iter_mut()
            .for_each(crate::images::redact_image);
    }
    Ok(Json(settings))
}

/// 설정의 주어진 값을 바꾼다. 아무것도 없으면 400. 설정이 아직 없으면 만든다(`If-Match`는 0).
/// `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이 바뀌었으면 다시 읽어 바꾼다. 같은 값이면 쓰지
/// 않고 그대로 200이다. enterprise 채널은 모든 노드가 이 빌드 이상이고 구독이 active일 때만 고른다
/// (이미 enterprise여도 다시 보내면 검사한다). 텔레메트리도 모든 노드가 이 빌드 이상일 때만 바꾸고,
/// 켤 때 id가 없으면 만든다. Prometheus 끝점은 모든 노드가 `metrics-history`를 알 때만 바꾸고(409
/// `cluster_upgrading`), 끄면 설정에서 지운다.
#[utoipa::path(patch, path = "/settings", tag = "settings",
    params(("If-Match" = Option<u64>, Header)),
    request_body = SettingsPatch,
    responses((status = 200, body = SettingsPatched), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    headers: HeaderMap,
    Json(req): Json<SettingsPatch>,
) -> Result<Json<SettingsPatched>, ApiError> {
    state.authorize(&principal, Action::Write, SettingsSpec::KIND)?;
    if req.updates.is_none()
        && req.telemetry.is_none()
        && req.web_origins.is_none()
        && req.api_names.is_none()
        && req.voters.is_none()
        && req.shutdown_policy.is_none()
        && req.ha_reserve.is_none()
        && req.ha_start_concurrency.is_none()
        && req.placement.is_none()
        && req.prometheus.is_none()
        && req.two_factor.is_none()
        && req.cpu.is_none()
        && req.acme.is_none()
        && req.history.is_none()
    {
        return Err(ApiError::bad_request(
            "give updates, telemetry, web_origins, api_names, voters, shutdown_policy, ha_reserve, ha_start_concurrency, placement, prometheus, two_factor, cpu, acme or history",
        ));
    }
    // 옛 노드는 설정 `history`를 풀지 못한다(계획 73, 검증보다 먼저).
    if req.history.is_some() {
        crate::cluster::require_feature(&state.cluster, opl_types::node::CONFIG_HISTORY)?;
    }
    if let Some(patch) = req.history
        && !patch.apply(HistorySettings::default()).valid()
    {
        return Err(ApiError::bad_request(
            "history: max_per_resource must be 10-1000, max_age_days 7-3650 and max_total_mib 8-512",
        ));
    }
    // 옛 노드는 설정 `acme`를 풀지 못한다(계획 73).
    if req.acme.is_some() {
        crate::cluster::require_feature(&state.cluster, opl_types::node::ACME)?;
    }
    // 옛 노드는 설정 `cpu`를 풀지 못한다(계획 63, 검증보다 먼저). 검증·별칭 고정은 쓰기 안에서 지금 설정에 합쳐 한다.
    let cpu_inv = match &req.cpu {
        Some(_) => {
            crate::cluster::require_feature(&state.cluster, CPU_MODELS)?;
            Some(crate::scheduler::Inventory::load(&state.cluster)?)
        }
        None => None,
    };
    let cpu_notes = std::sync::Mutex::new(Vec::new());
    let affected = std::sync::Mutex::new(None);
    // 옛 노드는 2단계 인증 정책을 풀지 못하고 대기 세션을 모른다.
    if let Some(policy) = &req.two_factor {
        crate::cluster::require_feature(&state.cluster, TWO_FACTOR)?;
        crate::two_factor::check_policy(&state.cluster, policy)?;
    }
    // 옛 노드는 Prometheus 필드를 풀지 못한다(계획 58·59처럼 검증보다 먼저 본다).
    if req.prometheus.is_some() {
        crate::cluster::require_feature(&state.cluster, METRICS_HISTORY)?;
    }
    if let Some(n) = req.placement.and_then(|p| p.threshold)
        && !(1..=MAX_PLACEMENT_THRESHOLD).contains(&n)
    {
        return Err(ApiError::bad_request(format!(
            "placement.threshold must be between 1 and {MAX_PLACEMENT_THRESHOLD} (got {n})"
        )));
    }
    if let Some(HaReservePatch::Set(r)) = req.ha_reserve
        && !r.valid()
    {
        return Err(ApiError::bad_request(format!(
            "ha_reserve must be nodes 1-{MAX_HA_RESERVE_NODES}, percent 1-{MAX_HA_RESERVE_PERCENT} or \"none\""
        )));
    }
    if let Some(n) = req.ha_start_concurrency
        && !(1..=MAX_HA_START_CONCURRENCY).contains(&n)
    {
        return Err(ApiError::bad_request(format!(
            "ha_start_concurrency must be between 1 and {MAX_HA_START_CONCURRENCY} (got {n})"
        )));
    }
    if let Some(n) = req.voters
        && !VOTER_LIMITS.contains(&n)
    {
        return Err(ApiError::bad_request(format!(
            "voters must be 3, 5 or 7 (got {n})"
        )));
    }
    let origins = req.web_origins.as_deref().map(web_origins).transpose()?;
    let names = req.api_names.as_deref().map(api_names).transpose()?;
    if let Some(endpoint) = req.telemetry.as_ref().and_then(|t| t.endpoint.as_deref())
        && !endpoint.is_empty()
    {
        check_endpoint(endpoint)?;
    }
    crate::cluster::require_upgraded(&state.cluster)?;
    let channel = req.updates.and_then(|u| u.channel);
    let enterprise = channel == Some(Channel::Enterprise);
    // 옛 노드는 enterprise 채널과 텔레메트리·허용 출처·API 이름·투표 멤버 상한 필드를 풀지 못한다.
    if enterprise
        || req.telemetry.is_some()
        || req.voters.is_some()
        || origins.as_ref().is_some_and(|o| !o.is_empty())
        || names.as_ref().is_some_and(|n| !n.is_empty())
    {
        crate::cluster::require_current(&state.cluster)?;
    }
    // 옛 노드는 종료 정책 필드를 풀지 못한다(버전이 섞여도 모든 노드가 알면 된다).
    if req.shutdown_policy.is_some() {
        crate::cluster::require_feature(&state.cluster, HA_PLACEMENT)?;
    }
    // 옛 노드는 HA 용량 예약·동시 시작 필드를 풀지 못한다.
    if req.ha_reserve.is_some() || req.ha_start_concurrency.is_some() {
        crate::cluster::require_feature(&state.cluster, FAST_HA)?;
    }
    // 옛 노드는 최적 배치 필드를 풀지 못한다.
    if req.placement.is_some() {
        crate::cluster::require_feature(&state.cluster, PLACEMENT)?;
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
        if let Some(origins) = &origins {
            spec.web_origins = origins.clone();
        }
        if let Some(names) = &names {
            spec.api_names = names.clone();
        }
        if let Some(n) = req.voters {
            spec.voters = Some(n);
        }
        // 기본(`conditional`)은 쓰지 않는다.
        if let Some(policy) = req.shutdown_policy {
            spec.shutdown_policy = (policy != ShutdownPolicy::Conditional).then_some(policy);
        }
        match req.ha_reserve {
            Some(HaReservePatch::Off(HaReserveOff::None)) => spec.ha_reserve = None,
            Some(HaReservePatch::Set(r)) => spec.ha_reserve = Some(r),
            None => {}
        }
        // 기본(4)은 쓰지 않는다.
        if let Some(n) = req.ha_start_concurrency {
            spec.ha_start_concurrency = (n != DEFAULT_HA_START_CONCURRENCY).then_some(n);
        }
        // 모두 기본값이면 쓰지 않는다.
        if let Some(patch) = req.placement {
            let placement = patch.apply(spec.placement());
            spec.placement = (placement != PlacementSettings::default()).then_some(placement);
        }
        // 꺼짐은 쓰지 않는다(없으면 꺼짐).
        if let Some(enabled) = req.prometheus.and_then(|p| p.enabled) {
            spec.prometheus = enabled.then_some(PrometheusSettings { enabled });
        }
        // 빈 정책은 쓰지 않는다.
        if let Some(policy) = &req.two_factor {
            spec.two_factor = (*policy != Default::default()).then(|| policy.clone());
        }
        // 모두 기본값이면 쓰지 않는다.
        if let (Some(patch), Some(inv)) = (&req.cpu, &cpu_inv) {
            let current = spec.cpu.clone().unwrap_or_default();
            let (next, notes) = crate::cpu::apply_settings(inv, &current, patch)?;
            *affected.lock().unwrap() = Some(crate::cpu::affected_running_vms(inv, &next));
            *cpu_notes.lock().unwrap() = notes;
            spec.cpu = (next != opl_types::cpu::CpuSettings::default()).then_some(next);
        }
        // 모두 기본값이면 쓰지 않는다.
        if let Some(patch) = req.acme {
            let mut acme = spec.acme();
            if let Some(v) = patch.http_redirect {
                acme.http_redirect = v;
            }
            if let Some(v) = patch.renew_jitter {
                acme.renew_jitter = v;
            }
            spec.acme = (acme != opl_types::settings::AcmeSettings::default()).then_some(acme);
        }
        // 모두 기본값이면 쓰지 않는다.
        if let Some(patch) = req.history {
            let history = patch.apply(spec.history());
            spec.history = (history != HistorySettings::default()).then_some(history);
        }
        Ok(())
    })
    .await?;
    let cpu_notes = cpu_notes.into_inner().unwrap();
    if !cpu_notes.is_empty() {
        audit.detail(format!("settings {}", cpu_notes.join(" ")));
    }
    let affected_running_vms = affected.into_inner().unwrap();
    if let Some(enabled) = req.prometheus.and_then(|p| p.enabled) {
        audit.detail(format!(
            "settings prometheus {}",
            if enabled { "on" } else { "off" }
        ));
    }
    if let Some(policy) = &req.two_factor {
        let roles: Vec<&str> = policy.roles.iter().map(|r| r.as_str()).collect();
        audit.detail(format!(
            "settings two-factor roles=[{}] realms=[{}]",
            roles.join(","),
            policy.realms.join(",")
        ));
    }
    let has_id = written
        .status
        .telemetry
        .as_ref()
        .is_some_and(|t| t.id.is_some());
    let patched = |settings| SettingsPatched {
        settings,
        affected_running_vms,
    };
    if !written.spec.telemetry.enabled || has_id {
        return Ok(Json(patched(public(written))));
    }
    // 처음 켰다: 보고에 붙일 id를 만든다(다른 요청이 먼저 만들었으면 그대로).
    let written = write_status(&state.cluster, |status| {
        let telemetry = status.telemetry.get_or_insert_default();
        if telemetry.id.is_none() {
            telemetry.id = Some(Uuid::new_v4());
        }
    })
    .await?;
    Ok(Json(patched(public(written))))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_compared_in_browser_form() {
        for (given, normalized) in [
            ("https://portal.example.com", "https://portal.example.com"),
            ("HTTPS://Portal.Example.COM", "https://portal.example.com"),
            (
                "https://portal.example.com:443",
                "https://portal.example.com",
            ),
            ("http://10.0.0.1:80", "http://10.0.0.1"),
            (
                "https://portal.example.com:8443",
                "https://portal.example.com:8443",
            ),
            ("http://[fd00::1]:8080", "http://[fd00::1]:8080"),
            ("https://[fd00::1]:443", "https://[fd00::1]"),
        ] {
            assert_eq!(
                normalize_origin(given).as_deref(),
                Some(normalized),
                "{given}"
            );
        }
        for bad in [
            "",
            "portal.example.com",
            "ftp://portal.example.com",
            "https://",
            "https://portal.example.com/",
            "https://portal.example.com/ui",
            "https://u@portal.example.com",
            "https://portal.example.com?x",
            "https://a b",
            "null",
        ] {
            assert_eq!(normalize_origin(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn api_names_are_dns_names_or_ips() {
        for (given, normalized) in [
            ("orbivisor.example.com", "orbivisor.example.com"),
            ("Orbivisor.Example.COM.", "orbivisor.example.com"),
            ("orbivisor", "orbivisor"),
            ("api-1.example.com", "api-1.example.com"),
            ("192.168.0.200", "192.168.0.200"),
            ("FD00:0::1", "fd00::1"),
        ] {
            assert_eq!(
                normalize_api_name(given).as_deref(),
                Some(normalized),
                "{given}"
            );
        }
        for bad in [
            "",
            ".",
            "*.example.com",
            "orbivisor.example.com:8006",
            "-a.example.com",
            "a-.example.com",
            "a..example.com",
            "a_b.example.com",
            "10.0.0",
            "999.1.1.1",
            "[fd00::1]",
            "https://orbivisor.example.com",
            &"a".repeat(64),
        ] {
            assert_eq!(normalize_api_name(bad), None, "{bad:?}");
        }
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            api_names(&list(&["A.example", "a.example", "10.0.0.1"])).unwrap(),
            list(&["a.example", "10.0.0.1"])
        );
        assert!(api_names(&list(&["a.example", "*.example"])).is_err());
        let many: Vec<String> = (0..=MAX_API_NAMES)
            .map(|i| format!("n{i}.example"))
            .collect();
        assert!(api_names(&many).is_err());
        assert!(api_names(&many[..MAX_API_NAMES]).is_ok());
    }

    #[test]
    fn web_origins_are_checked_and_deduplicated() {
        let list = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            web_origins(&list(&[
                "https://a.example",
                "HTTPS://A.example:443",
                "http://localhost:5173"
            ]))
            .unwrap(),
            list(&["https://a.example", "http://localhost:5173"])
        );
        assert!(web_origins(&list(&["https://a.example/"])).is_err());
        // http는 루프백만
        for ok in [
            "http://localhost",
            "http://127.0.0.1:8080",
            "http://[::1]:3000",
        ] {
            assert!(web_origins(&list(&[ok])).is_ok(), "{ok}");
        }
        for bad in [
            "http://b",
            "http://10.0.0.1",
            "http://localhost.example.com",
        ] {
            assert!(web_origins(&list(&[bad])).is_err(), "{bad}");
        }
        let many: Vec<String> = (0..=MAX_WEB_ORIGINS)
            .map(|i| format!("https://p{i}.example"))
            .collect();
        assert!(web_origins(&many).is_err());
        assert!(web_origins(&many[..MAX_WEB_ORIGINS]).is_ok());
    }
}
