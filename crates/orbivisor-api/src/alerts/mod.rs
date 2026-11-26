//! 경보 규칙·알림 대상·경보 읽기 API(계획 60 §8·§9·§12)와 경보 엔진.
//!
//! - `/alert-rules`: 모두 읽고 admin만 쓴다. `/notification-targets`: admin만 읽고 쓴다(비밀을 담는다). 응답에서 비밀을
//!   빼고 있는지만 보인다([`NotificationTargetView`]). 대상은 `/watch`로 내보내지 않는다.
//! - 쓰기는 모든 노드가 기능 `notifications`를 알 때만 받는다(409 `cluster_upgrading`, 검증보다 먼저).
//! - `GET /alerts`는 리더 경보(Raft)와 모든 노드의 노드 경보(`ReadAlerts`)를 합친다. `GET /alerts/history`는 모든 노드의
//!   보낸 기록을 시각 순으로 합친다.

mod book;
mod engine;
mod send;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use orbivisor_cluster::ClusterError;
use orbivisor_cluster::types::{NodeId, Rejection};
use orbivisor_types::alert::SubjectKind;
use orbivisor_types::alert::{
    AlertRule, AlertRuleSpec, AlertRuleStatus, AlertState, AlertView, AlertsQuery, AlertsReply,
    AlertsResponse, DEFAULT_HISTORY_LIMIT, HistoryWindow, MAX_HISTORY_LIMIT,
    NotificationHistoryResponse, NotificationState, NotificationTarget, NotificationTargetSpec,
    NotificationTargetView, NotificationTestResult, Severity, SmtpTarget, TargetConfig,
    WebhookTarget, redact_url,
};
use orbivisor_types::clock::{Clock, SystemClock};
use orbivisor_types::node::{NOTIFICATIONS, Node, NodeSpec};
use orbivisor_types::resource::{Resource, ResourceSpec};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

pub use book::{AlertBook, DIR as ALERTS_DIR, MAX_ACTIVE};
pub(crate) use engine::firing_counts;
pub use engine::{AlertEngine, AlertEngineConfig};
pub use send::SenderConfig;

use crate::audit::AuditNote;
use crate::auth::{Action, Principal};
use crate::cluster::require_feature;
use crate::error::{ApiError, Json, Query};
use crate::resources::{CreateRequest, List, UpdateRequest, check_name, if_match, lookup};
use crate::state::AppState;
use orbivisor_types::authz::Privilege;
use orbivisor_types::authz_eval::AuthzTarget;

/// 다른 노드의 경보를 읽는 데 기다리는 최대 시간
const PEER_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// 시험 보내기의 제한 시간
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 기록 읽기의 기본 범위(`until`부터 거슬러)
const DEFAULT_HISTORY_RANGE_MS: i64 = 24 * 3600 * 1000;

// ---- 규칙 ----

/// 기능(409, 검증보다 먼저), 형식(400), 대상이 있는지(400)
fn check_rule(state: &AppState, spec: &AlertRuleSpec) -> Result<(), ApiError> {
    require_feature(&state.cluster, NOTIFICATIONS)?;
    // SDN 조건(계획 67 §11)은 옛 노드가 풀지 못한다.
    if spec.condition.is_sdn() {
        require_feature(&state.cluster, orbivisor_types::node::SDN_SERVICES)?;
    }
    // 백업 저장소 조건(계획 68)도 옛 노드가 풀지 못한다.
    if spec.condition.is_backup_store() {
        require_feature(&state.cluster, orbivisor_types::node::BACKUP_STORE)?;
    }
    // 인증서 조건(계획 73 §11.6)은 옛 노드가 풀지 못한다.
    if spec.condition.is_acme() {
        require_feature(&state.cluster, orbivisor_types::node::ACME)?;
    }
    // 드리프트 조건(계획 73 §12.5)도 옛 노드가 풀지 못한다.
    if spec.condition.is_declarative() {
        require_feature(&state.cluster, orbivisor_types::node::DECLARATIVE)?;
    }
    // 링크 조건(계획 75 §3.4)도 옛 노드가 풀지 못한다.
    if spec.condition.is_link() {
        require_feature(&state.cluster, orbivisor_types::node::LINK_ALERTS)?;
    }
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    for t in &spec.targets {
        if state
            .cluster
            .get_by_name::<NotificationTargetSpec>(t)?
            .is_none()
        {
            return Err(ApiError::bad_request(format!(
                "notification target {t:?} not found"
            )));
        }
    }
    Ok(())
}

/// 경보 규칙을 만든다. 지표 임계값(`node_metric`·`vm_metric`, `for_secs` 동안 참이면 알림)·상태(`pool_usage`·
/// `node_offline`·`vm_failed`)·이벤트(`task_failed`·`ha_recovery`·`backup_job_failed`) 조건과 알림 대상 이름(1~16개)을
/// 받는다. admin만 쓰고 모든 노드가 `notifications`를 알아야 한다.
#[utoipa::path(post, path = "/alert-rules", tag = "alerts",
    request_body = CreateRequest<AlertRuleSpec>,
    responses((status = 201, body = Resource<AlertRuleSpec, AlertRuleStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_alert_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<CreateRequest<AlertRuleSpec>>,
) -> Result<(StatusCode, Json<AlertRule>), ApiError> {
    state.authorize(&principal, Action::Write, AlertRuleSpec::KIND)?;
    check_name(&req.name)?;
    audit.detail(format!("alert-rule create {}", req.name));
    check_rule(&state, &req.spec)?;
    let mut rule = Resource::new(req.name, req.spec);
    rule.labels = req.labels;
    rule.revision = state.cluster.create(&rule).await?;
    Ok((StatusCode::CREATED, Json(rule)))
}

/// 규칙을 통째로 바꾼다. 활성 경보는 새 설정으로 다시 판단한다(조건이 거짓이 되면 해제, 대상이 빠지면 알림 없이 뺀다).
#[utoipa::path(put, path = "/alert-rules/{key}", tag = "alerts",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = UpdateRequest<AlertRuleSpec>,
    responses((status = 200, body = Resource<AlertRuleSpec, AlertRuleStatus>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_alert_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
    Json(req): Json<UpdateRequest<AlertRuleSpec>>,
) -> Result<Json<AlertRule>, ApiError> {
    state.authorize(&principal, Action::Write, AlertRuleSpec::KIND)?;
    let rule = lookup::<AlertRuleSpec>(&state.cluster, &key)?;
    audit.detail(format!("alert-rule update {}", rule.name));
    check_rule(&state, &req.spec)?;
    state
        .cluster
        .update_spec(rule.id, if_match(&headers)?, req.labels, &req.spec)
        .await?;
    Ok(Json(lookup::<AlertRuleSpec>(
        &state.cluster,
        &rule.id.to_string(),
    )?))
}

/// 규칙을 지운다. 그 규칙의 활성 경보는 알림 없이 사라진다.
#[utoipa::path(delete, path = "/alert-rules/{key}", tag = "alerts",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_alert_rule(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, AlertRuleSpec::KIND)?;
    let rule = lookup::<AlertRuleSpec>(&state.cluster, &key)?;
    audit.detail(format!("alert-rule delete {}", rule.name));
    state
        .cluster
        .delete::<AlertRuleSpec>(rule.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /alert-rules/sdn-defaults` 본문
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SdnDefaultsRequest {
    /// 알릴 `NotificationTarget` 이름(1~16개)
    pub targets: Vec<String>,
}

/// `POST /alert-rules/sdn-defaults` 응답
#[derive(Debug, serde::Serialize, ToSchema)]
pub struct SdnDefaultsResponse {
    /// 만든 규칙
    pub created: Vec<Resource<AlertRuleSpec, AlertRuleStatus>>,
    /// 같은 이름의 규칙이 이미 있어 그대로 둔 것
    pub existing: Vec<String>,
}

/// "SDN 경보 켜기"(계획 67 §11): 권하는 SDN 규칙 여덟을 한 번에 만든다 — `sdn-quorum-lost` critical 0초,
/// `sdn-central-down`·`sdn-chassis-down` warning 60초, `sdn-gateway-unavailable` critical 30초, `sdn-gateway-failover` info,
/// `sdn-sync-failed` warning 120초, `sdn-physnet-not-ready` warning 60초, `sdn-lb-member-down` warning 30초. 같은 이름의
/// 규칙이 있으면 건드리지 않는다. 만든 뒤는 보통 규칙이다. admin만 쓰고 모든 노드가 `notifications`·`sdn-services`를 알아야
/// 한다.
#[utoipa::path(post, path = "/alert-rules/sdn-defaults", tag = "alerts",
    request_body = SdnDefaultsRequest,
    responses((status = 200, body = SdnDefaultsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_sdn_default_rules(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SdnDefaultsRequest>,
) -> Result<Json<SdnDefaultsResponse>, ApiError> {
    state.authorize(&principal, Action::Write, AlertRuleSpec::KIND)?;
    audit.detail(format!(
        "alert-rule sdn-defaults -> {}",
        req.targets.join(",")
    ));
    let rules = orbivisor_types::alert::sdn::default_rules(&req.targets);
    create_default_rules(&state, rules).await.map(Json)
}

/// "링크 경보 켜기"(계획 75 §3.4): 권하는 링크 규칙 둘을 한 번에 만든다 — `node-link-down`(업링크 포트) critical 30초,
/// `bond-degraded` warning 60초. SDN 묶음과 같은 방식이다: 같은 이름의 규칙이 있으면 건드리지 않고(다시 불러도 같다), 만든
/// 뒤는 보통 규칙이다. admin만 쓰고 모든 노드가 `notifications`·`link-alerts`를 알아야 한다(아니면 409 `cluster_upgrading`).
#[utoipa::path(post, path = "/alert-rules/link-defaults", tag = "alerts",
    request_body = SdnDefaultsRequest,
    responses((status = 200, body = SdnDefaultsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_link_default_rules(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<SdnDefaultsRequest>,
) -> Result<Json<SdnDefaultsResponse>, ApiError> {
    state.authorize(&principal, Action::Write, AlertRuleSpec::KIND)?;
    audit.detail(format!(
        "alert-rule link-defaults -> {}",
        req.targets.join(",")
    ));
    let rules = orbivisor_types::alert::link_default_rules(&req.targets);
    create_default_rules(&state, rules).await.map(Json)
}

/// 권하는 묶음 하나를 만든다. 대상·기능은 첫 규칙으로 한 번 본다(묶음 안의 규칙은 대상이 같고 같은 기능을 쓴다).
async fn create_default_rules(
    state: &AppState,
    rules: Vec<(&'static str, AlertRuleSpec)>,
) -> Result<SdnDefaultsResponse, ApiError> {
    if let Some((_, spec)) = rules.first() {
        check_rule(state, spec)?;
    }
    let mut out = SdnDefaultsResponse {
        created: Vec::new(),
        existing: Vec::new(),
    };
    for (name, spec) in rules {
        if state.cluster.get_by_name::<AlertRuleSpec>(name)?.is_some() {
            out.existing.push(name.to_string());
            continue;
        }
        let mut rule = Resource::new(name, spec);
        match state.cluster.create(&rule).await {
            Ok(revision) => {
                rule.revision = revision;
                out.created.push(rule);
            }
            // 그사이 같은 이름을 누가 만들었다.
            Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => {
                out.existing.push(name.to_string());
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

// ---- 알림 대상 ----

/// 알림 대상 만들기·바꾸기 본문. 바꾸기는 통째로 바꾸되 비밀은 다음처럼 지킨다(영역의 `bind_password`와 같은 규칙):
/// - SMTP `password`를 빼면 `host`·`port`·`user`가 모두 그대로일 때만 저장된 값을 두고(하나라도 바뀌면 400), 빈 글이면
///   지운다.
/// - webhook `url`에 응답의 줄인 URL(`scheme://host[:port]/…`)을 그대로 주면 저장된 URL을 둔다.
/// - webhook `token`을 빼면 URL의 출처(scheme·host·port)와 `ca_pem`이 그대로일 때만 저장된 값을 두고, 빈 글이면 지운다.
/// - webhook `headers`의 값이 빈 글이면 그 이름의 저장된 값을 둔다(출처·`ca_pem`이 그대로일 때만). 뺀 이름은 지운다.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationTargetRequest {
    /// 만들 때 필요하다. 바꿀 때 주면 지금 이름과 같아야 한다.
    #[serde(default)]
    #[schema(nullable = false, default)]
    pub name: Option<String>,
    pub target: TargetConfig,
    /// 끄면 보내지 않는다(시험 보내기는 된다).
    #[serde(default = "yes")]
    #[schema(default = true)]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// CRLF를 LF로 바꾸고 앞뒤 공백을 뗀다.
fn trim_pem(pem: Option<String>) -> Option<String> {
    pem.map(|p| p.replace("\r\n", "\n").trim().to_string())
        .filter(|p| !p.is_empty())
}

/// 요청을 저장할 spec으로. `stored`는 바꿀 때의 지금 값이다(빠진 비밀을 채운다).
fn target_spec(
    req: NotificationTargetRequest,
    stored: Option<&NotificationTargetSpec>,
) -> Result<NotificationTargetSpec, ApiError> {
    let target = match req.target {
        TargetConfig::Smtp(s) => {
            let old = match stored.map(|s| &s.target) {
                Some(TargetConfig::Smtp(old)) => Some(old),
                _ => None,
            };
            TargetConfig::Smtp(smtp_of(s, old)?)
        }
        TargetConfig::Webhook(w) => {
            let old = match stored.map(|s| &s.target) {
                Some(TargetConfig::Webhook(old)) => Some(old),
                _ => None,
            };
            TargetConfig::Webhook(webhook_of(w, old)?)
        }
    };
    let spec = NotificationTargetSpec {
        target,
        enabled: req.enabled,
    };
    spec.validate()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(spec)
}

fn smtp_of(mut s: SmtpTarget, old: Option<&SmtpTarget>) -> Result<SmtpTarget, ApiError> {
    s.host = s.host.trim().to_string();
    s.password = match s.password.take() {
        Some(p) if p.is_empty() => None,
        Some(p) => Some(p),
        None => match old {
            Some(old) if old.password.is_some() => {
                // 저장된 비밀번호를 다른 서버·계정으로, 다른 TLS 방식(평문으로 낮추기 포함)으로 보내지 않는다.
                if old.host != s.host
                    || old.port != s.port
                    || old.user != s.user
                    || old.tls != s.tls
                {
                    return Err(ApiError::bad_request(
                        "password must be given again when host, port, user or tls changes",
                    ));
                }
                old.password.clone()
            }
            _ => None,
        },
    };
    Ok(s)
}

fn webhook_of(
    mut w: WebhookTarget,
    old: Option<&WebhookTarget>,
) -> Result<WebhookTarget, ApiError> {
    w.url = w.url.trim().to_string();
    w.ca_pem = trim_pem(w.ca_pem);
    if let Some(old) = old
        && w.url == redact_url(&old.url)
    {
        w.url = old.url.clone();
    }
    // 저장된 비밀(토큰·헤더 값)은 같은 URL(줄인 URL을 그대로 보냈거나 저장된 URL 전체)·같은 CA로만 다시 보낸다.
    // 경로만 바꿔도 다시 묻는다(같은 호스트의 다른 경로가 받은 요청을 보여 주는 곳일 수 있다).
    let same = old.filter(|old| old.url == w.url && old.ca_pem == w.ca_pem);
    let again = |what: &str| {
        ApiError::bad_request(format!(
            "{what} must be given again when the url or ca_pem changes"
        ))
    };
    w.token = match w.token.take() {
        Some(t) if t.is_empty() => None,
        Some(t) => Some(t),
        None => match (old.and_then(|o| o.token.as_ref()), same) {
            (Some(_), Some(same)) if w.format.uses_token() => same.token.clone(),
            (Some(_), None) if w.format.uses_token() => return Err(again("token")),
            _ => None,
        },
    };
    for (name, value) in w.headers.iter_mut() {
        if !value.is_empty() {
            continue;
        }
        let stored = old.and_then(|o| o.headers.get(name));
        *value = match (stored, same) {
            (Some(_), Some(same)) => same.headers[name].clone(),
            (Some(_), None) => return Err(again(&format!("header {name}"))),
            (None, _) => {
                return Err(ApiError::bad_request(format!("header {name} has no value")));
            }
        };
    }
    Ok(w)
}

#[utoipa::path(get, path = "/notification-targets", tag = "alerts",
    responses((status = 200, body = List<NotificationTargetView>), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_notification_targets(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<List<NotificationTargetView>>, ApiError> {
    state.authorize(&principal, Action::Read, NotificationTargetSpec::KIND)?;
    let (revision, items) = state.cluster.list::<NotificationTargetSpec>()?;
    Ok(Json(List {
        revision,
        items: items
            .into_iter()
            .map(NotificationTargetView::from)
            .collect(),
    }))
}

/// 알림 대상을 만든다(SMTP 또는 webhook `json`·`slack`·`teams`·`ntfy`·`gotify`). admin만 쓴다. 비밀은 응답에 넣지 않는다.
#[utoipa::path(post, path = "/notification-targets", tag = "alerts",
    request_body = NotificationTargetRequest,
    responses((status = 201, body = NotificationTargetView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn create_notification_target(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    audit: AuditNote,
    Json(req): Json<NotificationTargetRequest>,
) -> Result<(StatusCode, Json<NotificationTargetView>), ApiError> {
    state.authorize(&principal, Action::Write, NotificationTargetSpec::KIND)?;
    let name = req
        .name
        .clone()
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    check_name(&name)?;
    audit.detail(format!("notification-target create {name}"));
    require_feature(&state.cluster, NOTIFICATIONS)?;
    let spec = target_spec(req, None)?;
    let mut target: NotificationTarget = Resource::new(name, spec);
    target.revision = state.cluster.create(&target).await?;
    Ok((StatusCode::CREATED, Json(target.into())))
}

#[utoipa::path(get, path = "/notification-targets/{key}", tag = "alerts",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = NotificationTargetView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn get_notification_target(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
) -> Result<Json<NotificationTargetView>, ApiError> {
    state.authorize(&principal, Action::Read, NotificationTargetSpec::KIND)?;
    Ok(Json(
        lookup::<NotificationTargetSpec>(&state.cluster, &key)?.into(),
    ))
}

/// 대상을 통째로 바꾼다(비밀 규칙은 [`NotificationTargetRequest`]). `If-Match`가 없으면 읽은 revision에 쓰고, 그 사이
/// 바뀌었으면 다시 읽어 바꾼다.
#[utoipa::path(put, path = "/notification-targets/{key}", tag = "alerts",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    request_body = NotificationTargetRequest,
    responses((status = 200, body = NotificationTargetView), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn update_notification_target(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
    Json(req): Json<NotificationTargetRequest>,
) -> Result<Json<NotificationTargetView>, ApiError> {
    state.authorize(&principal, Action::Write, NotificationTargetSpec::KIND)?;
    let expected = if_match(&headers)?;
    let first = lookup::<NotificationTargetSpec>(&state.cluster, &key)?;
    audit.detail(format!("notification-target update {}", first.name));
    require_feature(&state.cluster, NOTIFICATIONS)?;
    if let Some(name) = &req.name
        && *name != first.name
    {
        return Err(ApiError::bad_request(
            "the name of a notification target cannot be changed",
        ));
    }
    for _ in 0..10 {
        let target = lookup::<NotificationTargetSpec>(&state.cluster, &first.id.to_string())?;
        if let Some(expected) = expected
            && expected != target.revision
        {
            return Err(ClusterError::Rejected(Rejection::RevisionMismatch {
                expected,
                actual: target.revision,
            })
            .into());
        }
        let req = NotificationTargetRequest {
            name: None,
            target: req.target.clone(),
            enabled: req.enabled,
        };
        let spec = target_spec(req, Some(&target.spec))?;
        if spec == target.spec {
            return Ok(Json(target.into()));
        }
        match state
            .cluster
            .update_spec(
                target.id,
                Some(target.revision),
                target.labels.clone(),
                &spec,
            )
            .await
        {
            Ok(_) => {}
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. }))
                if expected.is_none() =>
            {
                continue;
            }
            Err(e) => return Err(e.into()),
        }
        return Ok(Json(
            lookup::<NotificationTargetSpec>(&state.cluster, &target.id.to_string())?.into(),
        ));
    }
    Err(ApiError::conflict(
        "busy",
        "the notification target kept changing while being updated; try again",
    ))
}

/// 대상을 지운다. 규칙이 쓰고 있으면 409 `in_use`(규칙에서 먼저 뺀다).
#[utoipa::path(delete, path = "/notification-targets/{key}", tag = "alerts",
    params(("key" = String, Path, description = "id or name"), ("If-Match" = Option<u64>, Header)),
    responses((status = 204, description = "Deleted"), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn delete_notification_target(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    headers: HeaderMap,
    audit: AuditNote,
) -> Result<StatusCode, ApiError> {
    state.authorize(&principal, Action::Write, NotificationTargetSpec::KIND)?;
    let target = lookup::<NotificationTargetSpec>(&state.cluster, &key)?;
    audit.detail(format!("notification-target delete {}", target.name));
    let users: Vec<String> = state
        .cluster
        .list::<AlertRuleSpec>()?
        .1
        .into_iter()
        .filter(|r| r.spec.targets.contains(&target.name))
        .map(|r| r.name)
        .collect();
    if !users.is_empty() {
        return Err(ApiError::conflict(
            "in_use",
            format!(
                "alert rules still use notification target {:?}: {}",
                target.name,
                users.join(", ")
            ),
        ));
    }
    state
        .cluster
        .delete::<NotificationTargetSpec>(target.id, if_match(&headers)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 대상으로 시험 알림을 이 노드에서 바로 한 번 보낸다(다시 하지 않음, 10초 제한, 꺼진 대상도 보냄). 결과는 이 노드의
/// 보낸 기록에 `test`로 남는다.
#[utoipa::path(post, path = "/notification-targets/{key}/test", tag = "alerts",
    params(("key" = String, Path, description = "id or name")),
    responses((status = 200, body = NotificationTestResult), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn test_notification_target(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Path(key): Path<String>,
    audit: AuditNote,
) -> Result<Json<NotificationTestResult>, ApiError> {
    state.authorize(&principal, Action::Operate, NotificationTargetSpec::KIND)?;
    let target = lookup::<NotificationTargetSpec>(&state.cluster, &key)?;
    audit.detail(format!("notification-target test {}", target.name));
    let node = state
        .cluster
        .get::<NodeSpec>(state.node)?
        .map_or_else(|| state.node.to_string(), |n| n.name);
    let at = SystemClock.now_ms();
    let n = send::Notification {
        cluster: engine::cluster_name(&state.cluster),
        state: NotificationState::Test,
        severity: Severity::Info,
        rule: None,
        condition: None,
        subject: None,
        value: None,
        threshold: None,
        since: None,
        at,
        detail: Some(format!(
            "test notification for target {} from node {node}",
            target.name
        )),
    };
    let result = send::deliver(&target.spec.target, &n, TEST_TIMEOUT).await;
    if let Err(e) = &result {
        tracing::info!(target = %target.name, error = %e, "a test notification failed");
    }
    if let Some(book) = state.alerts.clone() {
        let rec = send::record_of(&n, &target.name, at, result.clone());
        let _ = tokio::task::spawn_blocking(move || book.record(rec)).await;
    }
    Ok(Json(NotificationTestResult {
        ok: result.is_ok(),
        error: result.err(),
    }))
}

// ---- 경보 읽기 ----

/// 경보를 아는 노드(위트니스 제외) 하나에 묻는다. 이 노드면 장부를 직접 읽는다. 그 노드가 경보를 모르면 `Ok(None)`.
async fn read_node(
    state: &AppState,
    node: &Node,
    query: AlertsQuery,
) -> (String, Result<Option<AlertsReply>, String>) {
    let name = node.name.clone();
    if node.id == state.node {
        let Some(book) = state.alerts.clone() else {
            return (name, Ok(None));
        };
        let read = orbivisor_peer::AlertsSource::read(&book, query).await;
        return (name, read.map(Some));
    }
    if !node.status.features.iter().any(|f| f == NOTIFICATIONS) {
        return (name, Ok(None));
    }
    if !node.status.online {
        return (name, Err("the node is offline".into()));
    }
    let Some(peers) = &state.peers else {
        return (name, Err("no node-to-node connection".into()));
    };
    let Some(addr) = state
        .cluster
        .members()
        .addr(NodeId(node.id))
        .map(str::to_string)
    else {
        return (name, Err("not a cluster member".into()));
    };
    let read = tokio::time::timeout(
        PEER_READ_TIMEOUT,
        peers.read_alerts(NodeId(node.id), &addr, query, PEER_READ_TIMEOUT),
    )
    .await;
    let read = match read {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(status)) => Err(status.message().to_string()),
        Err(_) => Err("timed out".into()),
    };
    (name, read)
}

/// 위트니스를 뺀 모든 노드에 묻는다(`FAN_OUT`개씩). 노드 이름 순이다.
async fn gather(
    state: &AppState,
    query: AlertsQuery,
) -> Result<Vec<(String, Result<Option<AlertsReply>, String>)>, ApiError> {
    let mut nodes = state.cluster.list::<NodeSpec>()?.1;
    nodes.retain(|n| !n.spec.witness);
    nodes.sort_by(|a, b| a.name.cmp(&b.name));
    let reads = nodes
        .iter()
        .map(|n| read_node(state, n, query.clone()))
        .collect();
    Ok(crate::fan_out::gather(reads).await)
}

fn view(rule: &AlertRule, a: orbivisor_types::alert::ActiveAlert) -> AlertView {
    AlertView {
        rule: rule.id,
        rule_name: rule.name.clone(),
        severity: rule.spec.severity,
        condition: rule.spec.condition.kind().into(),
        subject: a.subject,
        state: a.state,
        since: a.since,
        value: a.value,
        threshold: rule.spec.condition.threshold(),
        last_sent: a.last_sent,
        clear_since: a.clear_since,
    }
}

/// 활성 경보(`pending` 포함): 리더 경보(Raft)와 모든 노드의 노드 경보를 합친다. `firing` 먼저, 심각도 높은 것 먼저,
/// 오래된 것 먼저다. 답하지 않은 노드는 `unreachable`(그 노드의 노드 경보는 보이지 않고 `node_offline` 경보가 대신
/// 보인다), 경보를 모르는 옛 노드는 `unsupported`다. 지운 규칙의 경보는 뺀다.
#[utoipa::path(get, path = "/alerts", tag = "alerts",
    responses((status = 200, body = AlertsResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_alerts(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<AlertsResponse>, ApiError> {
    let rules: std::collections::BTreeMap<uuid::Uuid, AlertRule> = state
        .cluster
        .list::<AlertRuleSpec>()?
        .1
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let mut alerts: Vec<AlertView> = rules
        .values()
        .flat_map(|r| r.status.active.iter().map(|a| view(r, a.clone())))
        .collect();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    let mut overflowing = Vec::new();
    for (name, read) in gather(&state, AlertsQuery::default()).await? {
        match read {
            Ok(Some(reply)) => {
                if reply.overflow {
                    overflowing.push(name);
                }
                alerts.extend(
                    reply
                        .active
                        .into_iter()
                        .filter_map(|a| rules.get(&a.rule).map(|r| view(r, a))),
                );
            }
            Ok(None) => unsupported.push(name),
            Err(e) => {
                tracing::warn!(node = %name, error = %e, "cannot read the alerts of a node");
                unreachable.push(name);
            }
        }
    }
    // 경보는 대상을 볼 수 있는 것만(대상의 `alert.read`, 노드·백업 작업은 루트, 계획 62)
    let owners = crate::authz::Owners::load(&state.cluster)?;
    let mut alerts = state.authz.filter_by(&principal, alerts, |a| {
        subject_owner(&state, &owners, a.subject.kind, Some(a.subject.id), None)
    })?;
    alerts.sort_by(|a, b| {
        (b.state == AlertState::Firing)
            .cmp(&(a.state == AlertState::Firing))
            .then(b.severity.cmp(&a.severity))
            .then(a.since.cmp(&b.since))
            .then(a.rule_name.cmp(&b.rule_name))
            .then(a.subject.name.cmp(&b.subject.name))
    });
    Ok(Json(AlertsResponse {
        alerts,
        unreachable,
        unsupported,
        overflowing,
    }))
}

/// 경보 대상(종류와 id 또는 이름)의 주인. 볼 권한은 그 대상의 `alert.read`다. 노드·백업 작업·SDN·백업 저장소·모르는
/// 대상은 루트다.
fn subject_owner(
    state: &AppState,
    owners: &crate::authz::Owners,
    kind: SubjectKind,
    id: Option<uuid::Uuid>,
    name: Option<&str>,
) -> crate::authz::Owned {
    use orbivisor_types::storage::StoragePoolSpec;
    use orbivisor_types::task::TaskSpec;
    use orbivisor_types::vm::VmSpec;
    let target = match kind {
        SubjectKind::Vm => {
            let id = id.or_else(|| {
                name.and_then(|n| state.cluster.get_by_name::<VmSpec>(n).ok().flatten())
                    .map(|v| v.id)
            });
            id.map(|id| owners.vm(id))
        }
        SubjectKind::Pool => {
            let pool = match (id, name) {
                (Some(id), _) => state.cluster.get::<StoragePoolSpec>(id).ok().flatten(),
                (None, Some(n)) => state
                    .cluster
                    .get_by_name::<StoragePoolSpec>(n)
                    .ok()
                    .flatten(),
                _ => None,
            };
            pool.map(|p| AuthzTarget::Object {
                kind: orbivisor_types::resource::Kind::StoragePool,
                id: p.id,
                folder: p.spec.folder,
            })
        }
        SubjectKind::Task => id
            .and_then(|id| state.cluster.get::<TaskSpec>(id).ok().flatten())
            .map(|t| owners.task(&t.spec).target),
        // 라우터·부하 분산기 경보(계획 67)는 그 객체(네트워크 폴더)의 `alert.read`다.
        SubjectKind::Router => id
            .and_then(|id| {
                state
                    .cluster
                    .get::<orbivisor_types::router::RouterSpec>(id)
                    .ok()
                    .flatten()
            })
            .map(|r| AuthzTarget::Object {
                kind: orbivisor_types::resource::Kind::Router,
                id: r.id,
                folder: r.spec.folder,
            }),
        SubjectKind::LoadBalancer => id
            .and_then(|id| {
                state
                    .cluster
                    .get::<orbivisor_types::load_balancer::LoadBalancerSpec>(id)
                    .ok()
                    .flatten()
            })
            .map(|l| AuthzTarget::Object {
                kind: orbivisor_types::resource::Kind::LoadBalancer,
                id: l.id,
                folder: l.spec.folder,
            }),
        // 백업 경보(계획 68)는 백업의 주인(VM, 지웠으면 백업한 때의 VM 폴더)의 `alert.read`다.
        SubjectKind::Backup => id
            .and_then(|id| {
                state
                    .cluster
                    .get::<orbivisor_types::backup::BackupSpec>(id)
                    .ok()
                    .flatten()
            })
            .map(|b| owners.backup(b.spec.vm, b.spec.vm_spec.folder)),
        SubjectKind::Node
        | SubjectKind::BackupJob
        | SubjectKind::Sdn
        | SubjectKind::BackupStore
        | SubjectKind::Certificate
        | SubjectKind::ApplySet => None,
    };
    crate::authz::Owned {
        target: target.unwrap_or(AuthzTarget::Root),
        read: Privilege::AlertRead,
    }
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub struct AlertHistoryParams {
    /// 이 시각부터(포함): 유닉스 밀리초나 RFC 3339. 기본은 `until`의 24시간 전
    pub since: Option<String>,
    /// 이 시각까지(포함): 유닉스 밀리초나 RFC 3339. 기본은 지금
    pub until: Option<String>,
    /// 최대 개수(1~5000, 기본 200)
    pub limit: Option<u32>,
}

/// 모든 노드가 보낸 알림 기록을 새 것부터 읽는다(보낸 노드의 `alerts/history.log`). `limit`보다 많으면 잘라
/// `truncated`다.
#[utoipa::path(get, path = "/alerts/history", tag = "alerts",
    params(AlertHistoryParams),
    responses((status = 200, body = NotificationHistoryResponse), (status = "4XX", body = crate::error::ErrorBody)))]
pub async fn list_alert_history(
    State(state): State<Arc<AppState>>,
    principal: Principal,
    Query(params): Query<AlertHistoryParams>,
) -> Result<Json<NotificationHistoryResponse>, ApiError> {
    // 알림 대상은 admin만 읽으므로 다른 사용자에게는 대상 이름과 오류 문구(서버·주소가 담김)를 보이지 않는다.
    let admin = state
        .authz
        .allows(&principal, Privilege::ClusterAdmin, AuthzTarget::Root);
    let ms = |field: &str, v: &str| {
        crate::audit::parse_time(field, v).map(|t| i64::try_from(t).unwrap_or(i64::MAX))
    };
    let until = match &params.until {
        Some(v) => ms("until", v)?,
        None => SystemClock.now_ms(),
    };
    let since = match &params.since {
        Some(v) => ms("since", v)?,
        None => until.saturating_sub(DEFAULT_HISTORY_RANGE_MS),
    };
    let limit = params.limit.unwrap_or(DEFAULT_HISTORY_LIMIT);
    if !(1..=MAX_HISTORY_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_HISTORY_LIMIT}"
        )));
    }
    let query = AlertsQuery {
        history: Some(HistoryWindow {
            since,
            until,
            limit,
        }),
    };
    let mut records = Vec::new();
    let mut unreachable = Vec::new();
    let mut unsupported = Vec::new();
    for (name, read) in gather(&state, query).await? {
        match read {
            Ok(Some(reply)) => records.extend(reply.history),
            Ok(None) => unsupported.push(name),
            Err(e) => {
                tracing::warn!(node = %name, error = %e, "cannot read the notification history of a node");
                unreachable.push(name);
            }
        }
    }
    // 규칙의 대상 종류와 대상 이름으로 볼 수 있는 기록만 남긴다(시험 보내기는 루트, 계획 62).
    let kinds: std::collections::HashMap<String, SubjectKind> = state
        .cluster
        .list::<AlertRuleSpec>()?
        .1
        .into_iter()
        .map(|r| (r.name, r.spec.condition.subject_kind()))
        .collect();
    let owners = crate::authz::Owners::load(&state.cluster)?;
    let mut records = state.authz.filter_by(&principal, records, |r| {
        match (r.rule.as_ref().and_then(|n| kinds.get(n)), &r.subject) {
            (Some(kind), Some(name)) => subject_owner(&state, &owners, *kind, None, Some(name)),
            _ => crate::authz::Owned {
                target: AuthzTarget::Root,
                read: Privilege::AlertRead,
            },
        }
    })?;
    records.sort_by(|a, b| b.at.cmp(&a.at).then(a.node.cmp(&b.node)));
    let truncated = records.len() > limit as usize;
    records.truncate(limit as usize);
    if !admin {
        for r in &mut records {
            r.target.clear();
            r.error = None;
        }
    }
    Ok(Json(NotificationHistoryResponse {
        records,
        unreachable,
        unsupported,
        truncated,
    }))
}
