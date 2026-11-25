//! 라우터(`Router`): 외부 네트워크·인터페이스 네트워크를 이름으로 가리킨다. 설명·외부·인터페이스·경로를 바꾼다(`PATCH`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::folder::FolderKind;
use opl_types::manifest::ManifestKind;
use opl_types::resource::ResourceSpec;
use opl_types::router::RouterSpec;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    Adapter, Done, Fut, Live, Update, bad, lives_of, names_at, refs_of, roundtrip, typed,
    write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::error::{ApiError, Json};
use crate::routing::{CreateRouter, RouterPatch, create_router, delete_router, patch_router};
use crate::state::AppState;

pub(crate) struct RouterAdapter;

/// 바뀐 칸만 담은 PATCH 본문. 없어진 칸은 `null`(외부·대상 떼기)이고, 설명은 빈 글(설명 지우기)이다.
pub(crate) fn patch_body(desired: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        let gone = if f == "description" {
            json!("")
        } else {
            Value::Null
        };
        out.insert(f.clone(), desired.get(f).cloned().unwrap_or(gone));
    }
    Value::Object(out)
}

impl Adapter for RouterAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Network)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<RouterSpec>(cluster, ManifestKind::Router)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<RouterSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["description", "external", "interfaces", "routes"])
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let external = spec
            .pointer("/external/network")
            .and_then(Value::as_str)
            .map(str::to_string);
        refs_of(
            ManifestKind::Network,
            names_at(spec, "interfaces", "network")
                .into_iter()
                .chain(external),
        )
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
            Privilege::NetworkConfig,
            RouterSpec::KIND,
            folder,
        )
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/routers/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/routers/{key}")
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
            let spec: RouterSpec = typed(&spec).map_err(bad)?;
            let (_, _, Json(router)) = Box::pin(create_router(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateRouter { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: router.id,
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
            let patch: RouterPatch = typed(&patch_body(&desired, &fields)).map_err(bad)?;
            Box::pin(patch_router(
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

    /// 인터페이스가 남은 라우터는 지울 수 없으므로(409 `router_in_use`) 먼저 인터페이스를 뗀다.
    fn delete<'a>(
        &'a self,
        state: &'a Arc<AppState>,
        principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let mut revision = live.revision;
            let has_interfaces = live
                .spec
                .get("interfaces")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty());
            if has_interfaces {
                let patch: RouterPatch = typed(&json!({ "interfaces": [] })).map_err(bad)?;
                let (_, Json(router)) = Box::pin(patch_router(
                    State(state.clone()),
                    principal.clone(),
                    AuditNote::detached(),
                    Path(live.id.to_string()),
                    super::if_match(revision),
                    Json(patch),
                ))
                .await?;
                revision = router.revision;
            }
            Box::pin(delete_router(
                State(state.clone()),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(revision),
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
        write_labels_as::<RouterSpec>(cluster, id, revision, labels)
    }
}
