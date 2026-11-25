//! 보안 그룹(`SecurityGroup` — 루트): 설명과 규칙 목록 통째를 바꾼다(`PATCH`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::security_group::SecurityGroupSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::resources::CreateRequest;
use crate::security_groups::{
    SecurityGroupPatch, create_security_group, delete_security_group, patch_security_group,
};
use crate::state::AppState;

pub(crate) struct GroupAdapter;

impl Adapter for GroupAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<SecurityGroupSpec>(cluster, ManifestKind::SecurityGroup)
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
        roundtrip::<SecurityGroupSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["description", "rules"])
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/security-groups"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/security-groups/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/security-groups/{key}")
    }

    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            Ok(
                match typed::<SecurityGroupSpec>(check.desired).map(|s| s.validate()) {
                    Ok(Err(e)) => {
                        vec![problem(Some("rules"), ApiError::bad_request(e.to_string()))]
                    }
                    _ => Vec::new(),
                },
            )
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
            let spec: SecurityGroupSpec = typed(&spec).map_err(bad)?;
            let (_, Json(group)) = Box::pin(create_security_group(
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
        _labels: BTreeMap<String, String>,
        desired: Value,
        fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let patch: SecurityGroupPatch =
                typed(&super::router::patch_body(&desired, &fields)).map_err(bad)?;
            Box::pin(patch_security_group(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(patch),
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
            Box::pin(delete_security_group(
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
        write_labels_as::<SecurityGroupSpec>(cluster, id, revision, labels)
    }
}
