//! 네트워크(`Network` — 단순·SDN): 바꿀 수 있는 것은 SDN 서브넷 목록뿐(`PATCH /networks/{key}`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::folder::FolderKind;
use opl_types::manifest::ManifestKind;
use opl_types::network::NetworkSpec;
use opl_types::resource::ResourceSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, bad, lives_of, problem, roundtrip, typed,
    write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::resources::{CreateRequest, create_network, delete_network, require_host_admin};
use crate::sdn::{NetworkPatch, patch_network};
use crate::state::AppState;

pub(crate) struct NetworkAdapter;

impl Adapter for NetworkAdapter {
    fn tree(&self) -> Option<FolderKind> {
        Some(FolderKind::Network)
    }

    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<NetworkSpec>(cluster, ManifestKind::Network)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<NetworkSpec>(store)
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["subnets"])
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        desired: &Value,
        folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        state.authz.require_new(
            principal,
            Privilege::NetworkConfig,
            NetworkSpec::KIND,
            folder,
        )?;
        if let Ok(spec) = typed::<NetworkSpec>(desired) {
            if spec.bridge.is_some() {
                require_host_admin(state, principal, "creating a bridge network")?;
            }
            if spec.is_provider() {
                require_host_admin(state, principal, "creating a provider network")?;
            }
        }
        Ok(())
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/networks/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/networks/{key}")
    }

    /// 만들기: 검증과 VNI 겹침(처리기와 같은 문구)
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let Ok(spec) = typed::<NetworkSpec>(check.desired) else {
                return Ok(out);
            };
            if let Err(e) = spec.validate() {
                out.push(problem(None, ApiError::bad_request(e.to_string())));
            }
            if check.live.is_none()
                && let Some(vni) = spec.vni()
            {
                let (_, networks) = check.state.cluster.list::<NetworkSpec>()?;
                if let Some(other) = networks.iter().find(|n| n.spec.vni() == Some(vni)) {
                    out.push(problem(
                        Some("vxlan"),
                        ApiError::conflict(
                            "vni_in_use",
                            format!("VNI {vni} is used by network {:?}", other.name),
                        ),
                    ));
                }
            }
            Ok(out)
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
            let spec: NetworkSpec = typed(&spec).map_err(bad)?;
            let (_, _, Json(network)) = Box::pin(create_network(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: network.id,
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
            let spec: NetworkSpec = typed(&desired).map_err(bad)?;
            Box::pin(patch_network(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(NetworkPatch {
                    subnets: spec.subnets,
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
            Box::pin(delete_network(
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
        write_labels_as::<NetworkSpec>(cluster, id, revision, labels)
    }
}
