//! 클러스터 설정(`Settings` — 하나뿐, 이름 `cluster`): `PATCH /settings`가 받는 칸만 다룬다. 구독 키와 이미지 카탈로그는
//! 제 API로 바꾸는 칸이라 매니페스트에 없다(지금 값 그대로 — 선언하면 막힘). 처음 적용은 PATCH가 설정을 만들고,
//! 지우기는 없다(가지치기도 건너뛴다). 투표 멤버 상한(`voters`)은 PATCH가 지우지 못하므로 선언하지 않으면 지금 값이다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use opl_cluster::Cluster;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::resource::Resource;
use opl_types::settings::{DEFAULT_VOTERS, SETTINGS_NAME, SettingsSpec};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, roundtrip, typed,
    without, write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::settings::{SettingsPatch, patch_settings};
use crate::state::AppState;

/// 제 API로 바꾸는 칸(매니페스트에 없다)
const SERVER: [&str; 2] = ["subscription", "image_catalog"];
/// `PATCH /settings`가 바꾸는 칸
const PATCHABLE: &[&str] = &[
    "updates",
    "telemetry",
    "web_origins",
    "api_names",
    "voters",
    "shutdown_policy",
    "ha_reserve",
    "ha_start_concurrency",
    "placement",
    "prometheus",
    "two_factor",
    "cpu",
    "history",
    "acme",
];

pub(crate) struct SettingsAdapter;

/// 바뀐 맨 위 칸의 PATCH 본문: 칸마다 원하는 값 전체(없어진 칸은 기본값)
fn patch_body(spec: &SettingsSpec, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        let v = match f.as_str() {
            "updates" => json!({ "channel": spec.updates.channel }),
            "telemetry" => json!({
                "enabled": spec.telemetry.enabled,
                "endpoint": spec.telemetry.endpoint.clone().unwrap_or_default(),
            }),
            "web_origins" => json!(spec.web_origins),
            "api_names" => json!(spec.api_names),
            "voters" => json!(spec.voters.unwrap_or(DEFAULT_VOTERS)),
            "shutdown_policy" => json!(spec.shutdown_policy()),
            "ha_reserve" => spec.ha_reserve.map_or(json!("none"), |r| json!(r)),
            "ha_start_concurrency" => json!(spec.ha_start_concurrency()),
            "placement" => {
                let p = spec.placement();
                json!({ "policy": p.policy, "mode": p.mode, "threshold": p.threshold })
            }
            "prometheus" => json!({ "enabled": spec.prometheus_enabled() }),
            "two_factor" => json!(spec.two_factor.clone().unwrap_or_default()),
            "cpu" => {
                let c = spec.cpu.clone().unwrap_or_default();
                json!({ "baseline": c.baseline, "pinned": c.pinned, "auto_flags": c.auto_flags, "mask": c.mask })
            }
            "history" => {
                let h = spec.history();
                json!({ "max_per_resource": h.max_per_resource, "max_age_days": h.max_age_days,
                        "max_total_mib": h.max_total_mib })
            }
            "acme" => {
                let a = spec.acme();
                json!({ "http_redirect": a.http_redirect, "renew_jitter": a.renew_jitter })
            }
            _ => continue,
        };
        out.insert(f.clone(), v);
    }
    Value::Object(out)
}

/// 원하는 설정을 PATCH로 쓴다.
async fn write(
    state: &Arc<AppState>,
    principal: &Principal,
    revision: Option<u64>,
    spec: &Value,
    fields: &[String],
) -> Result<(), ApiError> {
    let spec: SettingsSpec = typed(spec).map_err(bad)?;
    let patch: SettingsPatch = typed(&patch_body(&spec, fields)).map_err(bad)?;
    let mut headers = axum::http::HeaderMap::new();
    if let Some(r) = revision {
        headers = super::if_match(r);
    }
    Box::pin(patch_settings(
        State(state.clone()),
        principal.clone(),
        AuditNote::detached(),
        headers,
        Json(patch),
    ))
    .await?;
    Ok(())
}

impl Adapter for SettingsAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<SettingsSpec>(cluster, ManifestKind::Settings)
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        without(spec, &SERVER)
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        Ok(spec.clone())
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<SettingsSpec>(store)
    }

    /// 선언하지 않은 구독 키·카탈로그·투표 멤버 상한은 지금 값
    fn keep_server_fields(
        &self,
        _state: &AppState,
        declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        let Some(obj) = desired.as_object_mut() else {
            return;
        };
        for f in SERVER.iter().chain(&["voters"]) {
            if declared.get(*f).is_none_or(Value::is_null)
                && let Some(v) = live.get(*f)
            {
                obj.insert((*f).into(), v.clone());
            }
        }
    }

    fn updatable(&self) -> Update {
        Update::Fields(PATCHABLE)
    }

    fn deletable(&self) -> bool {
        false
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("PATCH", "/settings"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/settings")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/settings")
    }

    /// 이름은 `cluster`뿐, 만들 때 제 API의 칸은 쓸 수 없다.
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = Vec::new();
            if check.name != SETTINGS_NAME {
                out.push(Problem::error(
                    "invalid_request",
                    format!("the settings are named {SETTINGS_NAME:?}"),
                    Some("metadata.name".into()),
                ));
            }
            if check.live.is_none() {
                for f in SERVER {
                    if check.desired.get(f).is_some_and(|v| !v.is_null()) {
                        out.push(Problem::error(
                            "immutable",
                            format!("Settings: {f} is set through its own API, not a manifest"),
                            Some(f.into()),
                        ));
                    }
                }
            }
            Ok(out)
        })
    }

    /// 처음 적용: PATCH가 설정을 만든다(모두 기본값이면 기본 설정을 만든다 — 처리기가 쓰는 꼴과 같다).
    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        name: String,
        _labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            if name != SETTINGS_NAME {
                return Err(ApiError::bad_request(format!(
                    "the settings are named {SETTINGS_NAME:?}"
                )));
            }
            let want = roundtrip::<SettingsSpec>(&spec).map_err(bad)?;
            let default = roundtrip::<SettingsSpec>(&json!({})).map_err(bad)?;
            let fields: Vec<String> = PATCHABLE
                .iter()
                .filter(|f| want.get(**f) != default.get(**f))
                .map(|f| f.to_string())
                .collect();
            if !fields.is_empty() {
                write(state, principal, None, &want, &fields).await?;
            }
            let id = match state.cluster.get_by_name::<SettingsSpec>(SETTINGS_NAME)? {
                Some(s) => s.id,
                None => {
                    let created: Resource<SettingsSpec, _> =
                        Resource::new(SETTINGS_NAME, SettingsSpec::default());
                    state.cluster.create(&created).await?;
                    created.id
                }
            };
            Ok(Done { id, task: None })
        })
    }

    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        _labels: BTreeMap<String, String>,
        desired: Value,
        fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            write(state, principal, Some(live.revision), &desired, &fields).await?;
            Ok(Done {
                id: live.id,
                task: None,
            })
        })
    }

    fn delete<'a>(
        &'a self,
        _state: &'a Arc<AppState>,
        _principal: &'a Principal,
        _live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            Err(ApiError::bad_request(
                "the cluster settings cannot be deleted",
            ))
        })
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<SettingsSpec>(cluster, id, revision, labels)
    }

    fn create_carries_labels(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 없어진 칸은 기본값 전체로(PATCH의 부분 칸은 "그대로 둠"이라 다 적는다)
    #[test]
    fn patch_bodies_spell_out_whole_fields() {
        let spec: SettingsSpec =
            serde_json::from_value(json!({ "placement": { "mode": "auto" } })).unwrap();
        let fields: Vec<String> = ["placement", "ha_reserve", "history", "telemetry"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let body = patch_body(&spec, &fields);
        assert_eq!(
            body,
            json!({
                "placement": { "policy": "balance", "mode": "auto", "threshold": 15 },
                "ha_reserve": "none",
                "history": { "max_per_resource": 100, "max_age_days": 365, "max_total_mib": 64 },
                "telemetry": { "enabled": false, "endpoint": "" },
            })
        );
        let _: SettingsPatch = typed(&body).unwrap();
    }
}
