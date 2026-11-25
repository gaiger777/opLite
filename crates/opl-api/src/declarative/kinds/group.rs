//! 로컬 그룹(`Group` — 루트): 멤버는 사용자 이름(저장은 id — 사용자는 매니페스트 종류가 아니라 지금 있는 사용자에서
//! 푼다), 디렉터리 그룹(`external`)은 그대로. 바꾸기는 `PUT /groups/{key}` 통째(레이블도 싣는다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::GroupSpec;
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
use crate::permissions::{create_group, delete_group, update_group};
use crate::resources::{CreateRequest, UpdateRequest};
use crate::state::AppState;

pub(crate) struct GroupAdapter;

/// `members`의 값마다 `f`
fn map_members(
    spec: &Value,
    mut f: impl FnMut(&str) -> Result<String, String>,
) -> Result<Value, String> {
    let mut out = spec.clone();
    if let Some(list) = out.get_mut("members").and_then(Value::as_array_mut) {
        for m in list.iter_mut() {
            if let Some(s) = m.as_str() {
                *m = Value::String(f(s)?);
            }
        }
    }
    Ok(out)
}

impl Adapter for GroupAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<GroupSpec>(cluster, ManifestKind::Group)
    }

    /// 멤버 id → 사용자 이름
    fn to_manifest(&self, spec: &Value, world: &World) -> Value {
        map_members(spec, |s| {
            Ok(s.parse::<Uuid>()
                .map_or_else(|_| s.to_string(), |id| world.user_name(id)))
        })
        .expect("names map without errors")
    }

    /// 사용자 이름 → id(없는 사용자는 막힘)
    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        map_members(spec, |s| {
            world
                .user_id(s)
                .map(|id| id.to_string())
                .ok_or_else(|| format!("user {s:?} not found"))
        })
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<GroupSpec>(store)
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
        check_route(state, principal, ("POST", "/groups"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/groups/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/groups/{key}")
    }

    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            Ok(
                match typed::<GroupSpec>(check.desired).map(|s| s.validate()) {
                    Ok(Err(e)) => vec![problem(
                        Some("external"),
                        ApiError::bad_request(e.to_string()),
                    )],
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
            let spec: GroupSpec = typed(&spec).map_err(bad)?;
            let (_, Json(group)) = Box::pin(create_group(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
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
            let spec: GroupSpec = typed(&desired).map_err(bad)?;
            Box::pin(update_group(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
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
            Box::pin(delete_group(
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
        write_labels_as::<GroupSpec>(cluster, id, revision, labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn members_map_both_ways_and_keep_other_fields() {
        let id = Uuid::now_v7();
        let to_name = map_members(
            &json!({ "members": [id.to_string()], "description": "d" }),
            |s| {
                Ok(if s == id.to_string() {
                    "alice".into()
                } else {
                    s.into()
                })
            },
        )
        .unwrap();
        assert_eq!(to_name, json!({ "members": ["alice"], "description": "d" }));
        let e = map_members(&json!({ "members": ["bob"] }), |s| {
            Err(format!("user {s:?} not found"))
        })
        .unwrap_err();
        assert!(e.contains("bob"));
    }
}
