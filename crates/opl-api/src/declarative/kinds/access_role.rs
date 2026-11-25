//! 사용자 정의 역할(`AccessRole` — 루트): spec은 저장 꼴 그대로(`privileges`·`description`). 만들기·바꾸기 본문이 레이블을
//! 싣지 않아 레이블은 따로 쓴다. 내장 역할(`admin`·`operator`·`viewer`)은 저장하지 않으므로 다루지 않는다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::{AccessRoleSpec, validate_role_name};
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::permissions::{
    CreateAccessRoleRequest, UpdateAccessRoleRequest, create_access_role, delete_access_role,
    update_access_role,
};
use crate::state::AppState;

pub(crate) struct RoleAdapter;

impl Adapter for RoleAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<AccessRoleSpec>(cluster, ManifestKind::AccessRole)
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
        roundtrip::<AccessRoleSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn create_carries_labels(&self) -> bool {
        false
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
        check_route(state, principal, ("POST", "/access-roles"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/access-roles/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/access-roles/{key}")
    }

    /// 이름(내장 역할 이름은 쓸 수 없다)과 권한 이름
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = Vec::new();
            if let Err(e) = validate_role_name(check.name) {
                out.push(problem(None, ApiError::bad_request(e.to_string())));
            }
            if let Ok(spec) = typed::<AccessRoleSpec>(check.desired)
                && let Err(e) = spec.validate()
            {
                out.push(problem(
                    Some("privileges"),
                    ApiError::bad_request(e.to_string()),
                ));
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
            let spec: AccessRoleSpec = typed(&spec).map_err(bad)?;
            let (_, Json(view)) = Box::pin(create_access_role(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateAccessRoleRequest {
                    name,
                    privileges: spec.privileges,
                    description: spec.description,
                }),
            ))
            .await?;
            let id = view
                .id
                .ok_or_else(|| ApiError::internal("a made role has an id"))?;
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
        _fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: AccessRoleSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_access_role(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(UpdateAccessRoleRequest {
                    privileges: spec.privileges,
                    description: spec.description,
                }),
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
            Box::pin(delete_access_role(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
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
        write_labels_as::<AccessRoleSpec>(cluster, id, revision, labels)
    }
}
