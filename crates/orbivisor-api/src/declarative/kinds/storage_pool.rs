//! 스토리지 풀(`StoragePool`): `nodes`는 노드 이름(저장은 id). 바꿀 수 있는 것은 `shared`·`backups`뿐(`PATCH`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::authz::Privilege;
use orbivisor_types::folder::FolderKind;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use orbivisor_types::resource::ResourceSpec;
use orbivisor_types::storage::StoragePoolSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{Adapter, Done, Fut, Live, Update, World, lives_of, roundtrip, write_labels_as};
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::resources::{
    CreateRequest, PoolPatch, create_pool, delete_pool, patch_pool, require_host_admin,
};
use crate::state::AppState;

pub(crate) struct PoolAdapter;

/// `nodes`의 값을 `f`로 바꾼다(id ↔ 이름).
fn map_nodes(spec: &Value, f: impl Fn(&str) -> Result<String, String>) -> Result<Value, String> {
    let mut out = spec.clone();
    if let Some(nodes) = out.get_mut("nodes").and_then(Value::as_array_mut) {
        for n in nodes.iter_mut() {
            if let Some(s) = n.as_str() {
                *n = Value::String(f(s)?);
            }
        }
    }
    Ok(out)
}

impl Adapter for PoolAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Storage)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<StoragePoolSpec>(cluster, ManifestKind::StoragePool)
    }

    fn to_manifest(&self, spec: &Value, world: &World) -> Value {
        let spec = super::without(spec, &["folder"]);
        map_nodes(&spec, |s| {
            Ok(s.parse::<Uuid>()
                .map_or_else(|_| s.to_string(), |id| world.node_name(id)))
        })
        .unwrap_or(spec)
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        folder: Option<Uuid>,
        _state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        let spec = map_nodes(spec, |s| {
            world
                .node_id(s)
                .map(|id| id.to_string())
                .ok_or_else(|| format!("node {s:?} not found"))
        })?;
        Ok(super::with(
            &spec,
            "folder",
            folder.map(|f| Value::String(f.to_string())),
        ))
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<StoragePoolSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["shared", "backups"])
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        state.authz.require_new(
            principal,
            Privilege::StorageConfig,
            StoragePoolSpec::KIND,
            folder,
        )?;
        require_host_admin(state, principal, "creating a storage pool")
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/storage-pools/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/storage-pools/{key}")
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
            let (_, Json(pool)) = Box::pin(create_pool(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: pool.id,
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
            let field = |f: &str| {
                fields
                    .iter()
                    .any(|x| x == f)
                    .then(|| desired.get(f).and_then(Value::as_bool).unwrap_or(false))
            };
            let patch = PoolPatch {
                shared: field("shared"),
                backups: field("backups"),
            };
            Box::pin(patch_pool(
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
            Box::pin(delete_pool(
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

    /// 만들기 처리기가 `Cluster::create` 뒤에 id를 다시 쓰지 않는다(지운 것을 같은 id로 살린다).
    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn write_labels<'a>(
        &'a self,
        cluster: &'a Cluster,
        id: Uuid,
        revision: u64,
        labels: BTreeMap<String, String>,
    ) -> Fut<'a, ()> {
        write_labels_as::<StoragePoolSpec>(cluster, id, revision, labels)
    }
}
