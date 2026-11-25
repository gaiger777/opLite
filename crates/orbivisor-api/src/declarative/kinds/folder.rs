//! 폴더(`Folder`): `metadata.name`은 화면 이름, `metadata.folder`는 부모 경로, spec은 `{kind, quota?}`. 폴더의 열쇠는
//! 트리와 경로다(형제 사이에서만 이름이 겹치지 않는다). 이름·부모를 바꾸면 다른 폴더다 — 바꿀 수 있는 것은 할당량뿐.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::authz::Privilege;
use orbivisor_types::folder::{FolderKind, FolderQuota, FolderSpec};
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Adapter, Done, Fut, Live, Update, World, bad, roundtrip, typed, write_labels_as};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::folders::{
    CreateFolderRequest, FolderPatch, create_folder, delete_folder, parent_target, patch_folder,
    require_at,
};
use crate::state::AppState;

pub(crate) struct FolderAdapter;

/// 폴더 문서의 트리(`spec.kind`)
pub(crate) fn tree_of(spec: &Value) -> Result<FolderKind, String> {
    let kind = spec
        .get("kind")
        .ok_or_else(|| "spec.kind (vm, network or storage) is required for a Folder".to_string())?;
    serde_json::from_value(kind.clone()).map_err(|e| format!("spec.kind: {e}"))
}

impl Adapter for FolderAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        Ok(cluster
            .list::<FolderSpec>()?
            .1
            .iter()
            .map(|f| {
                let mut live = Live::of(ManifestKind::Folder, f);
                live.name = f.spec.name.clone();
                live
            })
            .collect())
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        super::without(spec, &["name", "parent"])
    }

    fn to_store(
        &self,
        spec: &Value,
        name: &str,
        folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        let spec = super::with(spec, "name", Some(json!(name)));
        Ok(super::with(
            &spec,
            "parent",
            folder.map(|f| Value::String(f.to_string())),
        ))
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<FolderSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["quota"])
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        require_at(
            state,
            principal,
            Privilege::InventoryFolder,
            parent_target(folder),
        )?;
        if desired.get("quota").is_some_and(|q| !q.is_null()) {
            require_at(
                state,
                principal,
                Privilege::InventoryQuota,
                parent_target(folder),
            )?;
        }
        Ok(())
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/folders/{id}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/folders/{id}")
    }

    /// 할당량은 부모(없으면 루트)의 `inventory.quota`(처리기와 같다)
    fn may_update_extra(
        &self,
        state: &AppState,
        principal: &Principal,
        live: &Live,
    ) -> Result<(), ApiError> {
        require_at(
            state,
            principal,
            Privilege::InventoryQuota,
            parent_target(live.folder()),
        )
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
            let spec: FolderSpec = typed(&spec).map_err(bad)?;
            let req = CreateFolderRequest {
                name,
                kind: spec.kind,
                parent: spec.parent,
                quota: spec.quota,
            };
            let (_, Json(folder)) = Box::pin(create_folder(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: folder.id,
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
            let quota: Option<FolderQuota> = match desired.get("quota") {
                None | Some(Value::Null) => None,
                Some(q) => Some(typed(q).map_err(bad)?),
            };
            let patch = FolderPatch {
                name: None,
                parent: None,
                quota: Some(quota),
            };
            Box::pin(patch_folder(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
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
            Box::pin(delete_folder(
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
        write_labels_as::<FolderSpec>(cluster, id, revision, labels)
    }

    fn create_carries_labels(&self) -> bool {
        false
    }
}
