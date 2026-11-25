//! DNS-01 공급자(`AcmeDns` — 루트): spec은 저장 꼴 그대로(`provider`·`propagation`·`zones`). 공급자 비밀(API 토큰·
//! 비밀 키·TSIG 비밀·PowerDNS 키)은 비밀 칸이다(`secretRef`면 저장한 값 그대로 — 내보낸 파일을 다시 적용해도 바뀌지
//! 않는다). 공급자 갈래를 바꾸면 그 칸 통째가 바뀐다. 바꾸기는 `PATCH`(공급자는 통째로).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::acme::AcmeDnsSpec;
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::secret::{missing, refs_to_keep, restore_kept};
use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::acme::api::{CreateDnsRequest, DnsPatch, check_dns, create_dns, delete_dns, patch_dns};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::state::AppState;

pub(crate) struct DnsAdapter;

/// 공급자 갈래마다의 비밀 칸(없는 갈래의 자리는 그냥 없다)
const SECRETS: &[&str] = &[
    "provider.api_token",
    "provider.secret_access_key",
    "provider.secret",
    "provider.api_key",
];

impl Adapter for DnsAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<AcmeDnsSpec>(cluster, ManifestKind::AcmeDns)
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
        let mut out = spec.clone();
        refs_to_keep(&mut out, SECRETS);
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<AcmeDnsSpec>(store)
    }

    /// 비밀은 같은 공급자 갈래일 때만 저장한 값을 쓴다(다른 갈래의 비밀을 옮기지 않는다).
    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        if desired.pointer("/provider/kind") == live.pointer("/provider/kind") {
            restore_kept(desired, live, SECRETS);
        }
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["provider", "propagation", "zones"])
    }

    fn secret_paths(&self) -> &'static [&'static str] {
        SECRETS
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
        check_route(state, principal, ("POST", "/acme-dns"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/acme-dns/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/acme-dns/{key}")
    }

    /// 풀리지 않은 비밀, 꼴과 이 노드에서 만들 수 있는지(TSIG base64·주소 — 처리기의 검사)
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let out = missing(check.desired, SECRETS);
            if !out.is_empty() {
                return Ok(out);
            }
            Ok(match typed::<AcmeDnsSpec>(check.desired) {
                Ok(spec) => match check_dns(check.state, &spec) {
                    Err(e) => vec![problem(Some("provider"), e)],
                    Ok(()) => Vec::new(),
                },
                Err(_) => Vec::new(),
            })
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
            let spec: AcmeDnsSpec = typed(&spec).map_err(bad)?;
            Box::pin(create_dns(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateDnsRequest {
                    name: name.clone(),
                    labels,
                    provider: spec.provider,
                    propagation: Some(spec.propagation),
                    zones: spec.zones,
                }),
            ))
            .await?;
            let id = state
                .cluster
                .get_by_name::<AcmeDnsSpec>(&name)?
                .map(|d| d.id)
                .ok_or_else(|| ApiError::not_found(format!("acme-dns {name:?} is gone")))?;
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
        fields: Vec<String>,
    ) -> Fut<'a, Done> {
        Box::pin(async move {
            let spec: AcmeDnsSpec = typed(&desired).map_err(bad)?;
            let has = |f: &str| fields.iter().any(|x| x == f);
            let patch = DnsPatch {
                provider: has("provider").then(|| spec.provider.clone()),
                propagation: has("propagation").then_some(spec.propagation),
                zones: has("zones").then(|| spec.zones.clone()),
            };
            Box::pin(patch_dns(
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
            Box::pin(delete_dns(
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
        write_labels_as::<AcmeDnsSpec>(cluster, id, revision, labels)
    }
}
