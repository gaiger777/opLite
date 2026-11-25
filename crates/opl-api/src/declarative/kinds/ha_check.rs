//! HA 검사(`HaCheck` — 루트): 노드·풀은 이름 그대로다(풀은 참조 — 같은 계획의 풀 뒤에 만든다). 바꾸기는
//! `PUT /ha-checks/{key}` 통째. 클러스터가 스스로 만드는 링크 검사(`auto-` 이름)는 다루지 않는다(내보내지 않음).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::ha_check::{AUTO_PREFIX, HaCheckSpec, HaCheckTarget};
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::ha_checks::{create_ha_check, delete_ha_check, update_ha_check};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;

pub(crate) struct HaCheckAdapter;

impl Adapter for HaCheckAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        let mut out = lives_of::<HaCheckSpec>(cluster, ManifestKind::HaCheck)?;
        out.retain(|l| !l.name.starts_with(AUTO_PREFIX));
        Ok(out)
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let Ok(spec) = typed::<HaCheckSpec>(spec) else {
            return Vec::new();
        };
        let pools = std::iter::once(&spec.target)
            .chain(spec.nodes.values())
            .filter_map(|t| match t {
                HaCheckTarget::Pool { name } => Some(name.clone()),
                _ => None,
            });
        super::refs_of(ManifestKind::StoragePool, pools)
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
        roundtrip::<HaCheckSpec>(store)
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
        check_route(state, principal, ("POST", "/ha-checks"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/ha-checks/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/ha-checks/{key}")
    }

    /// 꼴과 덮기 노드 이름(풀은 참조 검사가, 공유 풀인지는 처리기가 본다)
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let Ok(spec) = typed::<HaCheckSpec>(check.desired) else {
                return Ok(Vec::new());
            };
            if check.name.starts_with(AUTO_PREFIX) {
                return Ok(vec![Problem::error(
                    "invalid",
                    format!(
                        "names starting with {AUTO_PREFIX:?} are reserved for the automatic cluster link checks"
                    ),
                    None,
                )]);
            }
            if let Err(e) = spec.validate() {
                return Ok(vec![problem(None, ApiError::bad_request(e.to_string()))]);
            }
            let (_, nodes) = check.state.cluster.list::<opl_types::node::NodeSpec>()?;
            Ok(spec
                .nodes
                .keys()
                .filter(|n| !nodes.iter().any(|node| &node.name == *n))
                .map(|n| {
                    Problem::error(
                        "missing_reference",
                        format!("node {n:?} not found"),
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
            let spec: HaCheckSpec = typed(&spec).map_err(bad)?;
            let (_, Json(c)) = Box::pin(create_ha_check(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: c.id,
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
            let spec: HaCheckSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_ha_check(
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
            Box::pin(delete_ha_check(
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
        write_labels_as::<HaCheckSpec>(cluster, id, revision, labels)
    }
}
