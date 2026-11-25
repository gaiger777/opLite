//! ACME 계정(`AcmeAccount` — 루트): 계정 키는 서버가 만들고(만들기가 CA에 등록) 매니페스트에 없다 — 내보내지 않고
//! 견주지도 않는다. EAB HMAC 키는 비밀 칸(`secretRef`면 저장한 값 그대로). 바꿀 수 있는 것은 연락처·루트뿐(`PATCH` —
//! CA에 다시 등록해 본다), 디렉터리·EAB·약관 동의가 다르면 막힘(새 계정을 만든다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::acme::AcmeAccountSpec;
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::Value;
use uuid::Uuid;

use super::secret::{missing, refs_to_keep, restore_kept};
use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::acme::api::{
    AccountPatch, CreateAccountRequest, EabRequest, create_account, delete_account, patch_account,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::state::AppState;

pub(crate) struct AccountAdapter;

const SECRETS: &[&str] = &["eab.hmac_key"];

impl Adapter for AccountAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<AcmeAccountSpec>(cluster, ManifestKind::AcmeAccount)
    }

    /// 계정 키를 뺀다(서버가 만든다).
    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        super::without(spec, &["key"])
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        let mut out = super::without(spec, &["key"]);
        refs_to_keep(&mut out, SECRETS);
        Ok(out)
    }

    /// 견주는 꼴에는 계정 키가 없다.
    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<AcmeAccountSpec>(&super::without(store, &["key"]))
    }

    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        restore_kept(desired, live, SECRETS);
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["email", "ca_bundle"])
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
        check_route(state, principal, ("POST", "/acme-accounts"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/acme-accounts/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/acme-accounts/{key}")
    }

    /// 꼴(디렉터리·약관·EAB)과 풀리지 않은 비밀. CA 등록은 적용 때 처리기가 한다.
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = missing(check.desired, SECRETS);
            if let Ok(spec) = typed::<AcmeAccountSpec>(check.desired)
                && let Err(e) = spec.validate()
            {
                out.push(problem(None, ApiError::bad_request(e.to_string())));
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
            let spec: AcmeAccountSpec = typed(&spec).map_err(bad)?;
            Box::pin(create_account(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(CreateAccountRequest {
                    name: name.clone(),
                    labels,
                    directory: spec.directory,
                    email: spec.email,
                    agree_tos: spec.agree_tos,
                    eab: spec.eab.map(|e| EabRequest {
                        kid: e.kid,
                        hmac_key: e.hmac_key.expose().to_string(),
                    }),
                    ca_bundle: spec.ca_bundle,
                }),
            ))
            .await?;
            let id = state
                .cluster
                .get_by_name::<AcmeAccountSpec>(&name)?
                .map(|a| a.id)
                .ok_or_else(|| ApiError::not_found(format!("acme-account {name:?} is gone")))?;
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
            let spec: AcmeAccountSpec = typed(&desired).map_err(bad)?;
            let has = |f: &str| fields.iter().any(|x| x == f);
            let patch = AccountPatch {
                email: has("email").then(|| spec.email.clone()),
                ca_bundle: has("ca_bundle").then(|| spec.ca_bundle.clone()),
            };
            Box::pin(patch_account(
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
            Box::pin(delete_account(
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
        write_labels_as::<AcmeAccountSpec>(cluster, id, revision, labels)
    }
}
