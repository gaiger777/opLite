//! 배치 규칙(`PlacementRule` — 루트): VM·노드는 이름이다. 바꾸기는 `PUT /placement-rules/{key}` 통째.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::placement::{PlacementRule, PlacementRuleSpec};
use opl_types::resource::Resource;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, refs_of,
    roundtrip, typed, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::placement_rules::{create_placement_rule, delete_placement_rule, update_placement_rule};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;

pub(crate) struct RuleAdapter;

impl Adapter for RuleAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<PlacementRuleSpec>(cluster, ManifestKind::PlacementRule)
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
        roundtrip::<PlacementRuleSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn update_carries_labels(&self) -> bool {
        true
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let vms = spec
            .get("vms")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string));
        refs_of(ManifestKind::Vm, vms)
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/placement-rules"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/placement-rules/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/placement-rules/{key}")
    }

    /// 처리기의 검사(같은 계획이 만들 VM은 아직 없어 그 이름 검사를 건너뛴다)
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let Ok(spec) = typed::<PlacementRuleSpec>(check.desired) else {
                return Ok(Vec::new());
            };
            let planned_vm = spec.vms.iter().any(|v| {
                check
                    .planned
                    .docs
                    .contains_key(&(ManifestKind::Vm, v.clone()))
            });
            if planned_vm {
                return Ok(match spec.validate() {
                    Err(e) => vec![problem(None, ApiError::bad_request(e.to_string()))],
                    Ok(()) => Vec::new(),
                });
            }
            let mut rule: PlacementRule = Resource::new(check.name.to_string(), spec);
            if let Some(live) = check.live {
                rule.id = live.id;
            }
            Ok(match crate::placement_rules::check(check.state, &rule) {
                Err(e) => vec![problem(None, e)],
                Ok(()) => Vec::new(),
            })
        })
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
            let spec: PlacementRuleSpec = typed(&spec).map_err(bad)?;
            let (_, Json(rule)) = Box::pin(create_placement_rule(
                State(state.clone()),
                principal.clone(),
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
            let spec: PlacementRuleSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_placement_rule(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
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
            Box::pin(delete_placement_rule(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
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
        write_labels_as::<PlacementRuleSpec>(cluster, id, revision, labels)
    }
}
