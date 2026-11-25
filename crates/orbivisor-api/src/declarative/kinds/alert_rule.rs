//! 경보 규칙(`AlertRule` — 루트): 알림 대상은 이름이다. 바꾸기는 `PUT /alert-rules/{key}` 통째(레이블도 싣는다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::alert::AlertRuleSpec;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Done, Fut, Live, Update, World, bad, check_route, lives_of, refs_of, roundtrip, typed,
    write_labels_as,
};
use crate::alerts::{create_alert_rule, delete_alert_rule, update_alert_rule};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;

pub(crate) struct AlertRuleAdapter;

impl Adapter for AlertRuleAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<AlertRuleSpec>(cluster, ManifestKind::AlertRule)
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        spec.clone()
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
        roundtrip::<AlertRuleSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn update_carries_labels(&self) -> bool {
        true
    }

    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let targets = spec
            .get("targets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string));
        refs_of(ManifestKind::NotificationTarget, targets)
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/alert-rules"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/alert-rules/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/alert-rules/{key}")
    }

    fn create<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        name: String,
        labels: BTreeMap<String, String>,
        spec: Value,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: AlertRuleSpec = typed(&spec).map_err(bad)?;
            let (_, Json(rule)) = Box::pin(create_alert_rule(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: rule.id,
                task: None,
            })
        })
    }

    fn update<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
        labels: BTreeMap<String, String>,
        desired: Value,
        _fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: AlertRuleSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_alert_rule(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                AuditNote::detached(),
                Json(UpdateRequest { labels, spec }),
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
            Box::pin(delete_alert_rule(
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
        write_labels_as::<AlertRuleSpec>(cluster, id, revision, labels)
    }
}
