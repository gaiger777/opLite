//! 알림 대상(`NotificationTarget` — 루트, SMTP·webhook): SMTP 비밀번호·webhook 토큰은 비밀 칸이다. webhook URL은 보기처럼
//! 줄인 URL(`https://host/…`)로, 헤더 값은 빈 글로 내보낸다 — 처리기가 그 둘을 "저장한 값 그대로"로 받는 규칙과 같다.
//! 바꾸기는 `PUT /notification-targets/{key}` 통째(레이블은 따로 쓴다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::alert::{NotificationTargetSpec, redact_url};
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, roundtrip, secret,
    typed, with, write_labels_as,
};
use crate::alerts::{
    NotificationTargetRequest, create_notification_target, delete_notification_target,
    update_notification_target,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::state::AppState;

const SECRETS: &[&str] = &["target.password", "target.token"];

pub(crate) struct TargetAdapter;

fn is_webhook(spec: &Value) -> bool {
    spec.pointer("/target/kind").and_then(Value::as_str) == Some("webhook")
}

/// 저장 spec(+ 이름) → 처리기 본문
fn request(desired: &Value, name: Option<&str>) -> Result<NotificationTargetRequest, ApiError> {
    secret::ensure_resolved(desired, SECRETS)?;
    typed(&with(
        desired,
        "name",
        name.map(|n| Value::String(n.into())),
    ))
    .map_err(bad)
}

/// 만들기에는 줄인 URL·빈 헤더 값을 채울 저장한 값이 없다.
fn unresolved(desired: &Value) -> Vec<Problem> {
    let mut out = secret::missing(desired, SECRETS);
    if !is_webhook(desired) {
        return out;
    }
    if desired
        .pointer("/target/url")
        .and_then(Value::as_str)
        .is_some_and(|u| u.ends_with("/…"))
    {
        out.push(Problem::error(
            "secret_missing",
            "target.url is the shortened URL but nothing is stored to keep; give the full URL",
            Some("target.url".into()),
        ));
    }
    if let Some(h) = desired
        .pointer("/target/headers")
        .and_then(Value::as_object)
    {
        for (k, _) in h.iter().filter(|(_, v)| v.as_str() == Some("")) {
            out.push(Problem::error(
                "secret_missing",
                format!("header {k} has no value and nothing is stored to keep; give the value"),
                Some(format!("target.headers.{k}")),
            ));
        }
    }
    out
}

/// 저장 spec → 매니페스트 spec(줄인 URL·빈 헤더 값)
fn export(spec: &Value) -> Value {
    let mut out = spec.clone();
    if is_webhook(&out) {
        if let Some(url) = out.pointer_mut("/target/url")
            && let Some(s) = url.as_str()
        {
            *url = Value::String(redact_url(s));
        }
        if let Some(h) = out
            .pointer_mut("/target/headers")
            .and_then(Value::as_object_mut)
        {
            for v in h.values_mut() {
                *v = Value::String(String::new());
            }
        }
    }
    out
}

/// 비밀 참조·줄인 URL·빈 헤더 값을 지금 저장한 값으로
fn keep(desired: &mut Value, live: &Value) {
    secret::restore_kept(desired, live, SECRETS);
    if !is_webhook(desired) || !is_webhook(live) {
        return;
    }
    if let (Some(url), Some(stored)) = (
        desired.pointer_mut("/target/url"),
        live.pointer("/target/url").and_then(Value::as_str),
    ) && url.as_str() == Some(redact_url(stored).as_str())
    {
        *url = Value::String(stored.to_string());
    }
    let stored = live
        .pointer("/target/headers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(h) = desired
        .pointer_mut("/target/headers")
        .and_then(Value::as_object_mut)
    {
        for (k, v) in h.iter_mut() {
            if v.as_str() == Some("")
                && let Some(s) = stored.get(k)
            {
                *v = s.clone();
            }
        }
    }
}

impl Adapter for TargetAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<NotificationTargetSpec>(cluster, ManifestKind::NotificationTarget)
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        export(spec)
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        let mut out = spec.clone();
        secret::refs_to_keep(&mut out, SECRETS);
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<NotificationTargetSpec>(store)
    }

    /// 비밀 참조·줄인 URL·빈 헤더 값을 지금 저장한 값으로
    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        keep(desired, live);
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn secret_paths(&self) -> &'static [&'static str] {
        SECRETS
    }

    /// 줄인 URL·빈 헤더 값(내보내기와 같다)
    fn redact_declared(&self, spec: &mut Value) {
        *spec = export(spec);
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/notification-targets"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/notification-targets/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/notification-targets/{key}")
    }

    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = unresolved(check.desired);
            if out.is_empty()
                && let Ok(spec) = typed::<NotificationTargetSpec>(check.desired)
                && let Err(e) = spec.validate()
            {
                out.push(super::problem(None, ApiError::bad_request(e.to_string())));
            }
            Ok(out)
        })
    }

    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        name: String,
        _labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let req = request(&spec, Some(&name))?;
            let (_, Json(target)) = Box::pin(create_notification_target(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: target.id,
                task: None,
            })
        })
    }

    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        _labels: BTreeMap<String, String>,
        desired: Value,
        _fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let req = request(&desired, None)?;
            Box::pin(update_notification_target(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                AuditNote::detached(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            Box::pin(delete_notification_target(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                AuditNote::detached(),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<NotificationTargetSpec>(cluster, id, revision, labels)
    }

    fn create_carries_labels(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 내보낸 webhook(줄인 URL·빈 헤더 값·토큰 참조)은 지금 값으로 돌아와 같다. 만들기에는 막힌다.
    #[test]
    fn exported_webhooks_keep_the_stored_url_headers_and_token() {
        let live = json!({ "target": { "kind": "webhook", "url": "https://hooks.example/T0/secret",
            "format": "json", "headers": { "X-Key": "k3y" }, "token": "t0k" } });
        let mut m = export(&live);
        assert_eq!(m["target"]["url"], "https://hooks.example/…");
        assert_eq!(m["target"]["headers"]["X-Key"], "");
        m["target"]["token"] = json!({ "secretRef": "" });
        let mut store = m.clone();
        secret::refs_to_keep(&mut store, SECRETS);
        assert_eq!(unresolved(&store).len(), 3);
        keep(&mut store, &live);
        let a = TargetAdapter;
        assert_eq!(a.normalize(&store).unwrap(), a.normalize(&live).unwrap());
        assert!(unresolved(&store).is_empty());
    }
}
