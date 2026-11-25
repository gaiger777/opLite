//! HA 묶음(`HaGroup` — 루트): 노드는 이름 그대로다. 바꾸기는 `PUT /ha-groups/{key}` 통째(레이블도 싣는다). VM의
//! `ha.group`이 이 이름을 가리키므로 VM보다 먼저 만든다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::placement::HaGroupSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::ha_groups::{create_ha_group, delete_ha_group, update_ha_group};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;

pub(crate) struct HaGroupAdapter;

impl Adapter for HaGroupAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<HaGroupSpec>(cluster, ManifestKind::HaGroup)
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
        roundtrip::<HaGroupSpec>(store)
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

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/ha-groups"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/ha-groups/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/ha-groups/{key}")
    }

    /// 꼴과 노드 이름(처리기의 검사)
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let Ok(spec) = typed::<HaGroupSpec>(check.desired) else {
                return Ok(Vec::new());
            };
            if let Err(e) = spec.validate() {
                return Ok(vec![problem(
                    Some("nodes"),
                    ApiError::bad_request(e.to_string()),
                )]);
            }
            let (_, nodes) = check.state.cluster.list::<opl_types::node::NodeSpec>()?;
            Ok(spec
                .nodes
                .iter()
                .filter(|n| !nodes.iter().any(|node| node.name == n.node))
                .map(|n| {
                    Problem::error(
                        "missing_reference",
                        format!("node {:?} not found", n.node),
                        Some("nodes".into()),
                    )
                })
                .collect())
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
            let spec: HaGroupSpec = typed(&spec).map_err(bad)?;
            let (_, Json(group)) = Box::pin(create_ha_group(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: group.id,
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
            let spec: HaGroupSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_ha_group(
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
            Box::pin(delete_ha_group(
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
        write_labels_as::<HaGroupSpec>(cluster, id, revision, labels)
    }
}
