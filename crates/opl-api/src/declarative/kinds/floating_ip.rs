//! 유동 IP(`FloatingIp`): 대상 VM은 이름(저장은 id). 서버가 고른 주소는 선언하지 않으면 그대로 둔다. 대상·설명을
//! 바꾼다(`PATCH`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::floating_ip::FloatingIpSpec;
use opl_types::folder::FolderKind;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::resource::ResourceSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Done, Fut, Live, Update, World, bad, lives_of, roundtrip, typed, write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::authz_routes::{Obj, resolve};
use crate::error::{ApiError, Json};
use crate::routing::{
    CreateFloatingIp, FloatingIpPatch, create_floating_ip, delete_floating_ip, patch_floating_ip,
};
use crate::state::AppState;

pub(crate) struct FipAdapter;

impl Adapter for FipAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Network)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<FloatingIpSpec>(cluster, ManifestKind::FloatingIp)
    }

    fn to_manifest(&self, spec: &Value, world: &World) -> Value {
        let mut out = super::without(spec, &["folder"]);
        if let Some(vm) = out.pointer_mut("/target/vm")
            && let Some(id) = vm.as_str().and_then(|s| s.parse::<Uuid>().ok())
        {
            *vm = Value::String(world.vm_name(id));
        }
        out
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        folder: Option<Uuid>,
        _state: Option<ManifestState>,
        world: &World,
    ) -> Result<Value, String> {
        let mut out = super::with(spec, "folder", folder.map(|f| Value::String(f.to_string())));
        if let Some(vm) = out.pointer_mut("/target/vm")
            && let Some(name) = vm.as_str().map(str::to_string)
        {
            // 아직 없는 VM(같은 계획에서 먼저 만든다)은 적용 때 다시 푼다.
            *vm = Value::String(
                world
                    .vm_id(&name)
                    .map_or_else(|| Uuid::nil().to_string(), |id| id.to_string()),
            );
        }
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<FloatingIpSpec>(store)
    }

    fn keep_server_fields(
        &self,
        _state: &AppState,
        declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        if declared.get("address").is_none()
            && let (Some(obj), Some(addr)) = (desired.as_object_mut(), live.get("address"))
        {
            obj.insert("address".into(), addr.clone());
        }
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["target", "description"])
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let mut out = Vec::new();
        if let Some(n) = spec.get("network").and_then(Value::as_str) {
            out.push((ManifestKind::Network, n.to_string()));
        }
        if let Some(vm) = spec.pointer("/target/vm").and_then(Value::as_str) {
            out.push((ManifestKind::Vm, vm.to_string()));
        }
        out
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
            FloatingIpSpec::KIND,
            folder,
        )
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/floating-ips/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/floating-ips/{key}")
    }

    /// `PATCH`는 경로표가 `Handler`다: 유동 IP의 `network.config`를 여기서 본다(대상 VM은 처리기가 본다).
    fn may_update_extra(
        &self,
        state: &AppState,
        principal: &Principal,
        live: &Live,
    ) -> Result<(), ApiError> {
        match resolve(&state.cluster, Obj::FloatingIp, &live.id.to_string())? {
            Some(found) => state
                .authz
                .require(principal, Privilege::NetworkConfig, &found),
            None => Err(ApiError::not_found(format!(
                "floating-ip {:?} not found",
                live.name
            ))),
        }
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
            let spec: FloatingIpSpec = typed(&spec).map_err(bad)?;
            let (_, Json(fip)) = Box::pin(create_floating_ip(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateFloatingIp {
                    name: Some(name),
                    labels,
                    spec,
                }),
            ))
            .await?;
            Ok(Done {
                id: fip.id,
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
            let patch: FloatingIpPatch =
                typed(&super::router::patch_body(&desired, &fields)).map_err(bad)?;
            Box::pin(patch_floating_ip(
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
        _principal: &'a Principal,
        live: &'a Live,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            Box::pin(delete_floating_ip(
                State(state.clone()),
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
        write_labels_as::<FloatingIpSpec>(cluster, id, revision, labels)
    }
}
