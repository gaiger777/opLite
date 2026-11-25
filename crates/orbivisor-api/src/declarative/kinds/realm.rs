//! 인증 영역(`Realm` — 루트, LDAP·OIDC): 서비스 계정 비밀번호·클라이언트 비밀은 비밀 칸이다. 바꾸기는
//! `PUT /realms/{key}` 통째(본문 `RealmRequest`는 저장 spec과 같은 칸 — 레이블은 따로 쓴다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use orbivisor_types::realm::RealmSpec;
use serde_json::Value;
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, roundtrip, secret,
    typed, with, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::realms::{RealmRequest, create_realm, delete_realm, update_realm};
use crate::state::AppState;

const SECRETS: &[&str] = &["backend.bind_password", "backend.client_secret"];

pub(crate) struct RealmAdapter;

/// 저장 spec(+ 이름) → 처리기 본문
fn request(desired: &Value, name: Option<&str>) -> Result<RealmRequest, ApiError> {
    secret::ensure_resolved(desired, SECRETS)?;
    typed(&with(
        desired,
        "name",
        name.map(|n| Value::String(n.into())),
    ))
    .map_err(bad)
}

impl Adapter for RealmAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<RealmSpec>(cluster, ManifestKind::Realm)
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
        secret::refs_to_keep(&mut out, SECRETS);
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<RealmSpec>(store)
    }

    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        secret::restore_kept(desired, live, SECRETS);
    }

    fn updatable(&self) -> Update {
        Update::Whole
    }

    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn secret_paths(&self) -> &'static [&'static str] {
        SECRETS
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/realms"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/realms/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/realms/{key}")
    }

    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = secret::missing(check.desired, SECRETS);
            if out.is_empty()
                && let Ok(spec) = typed::<RealmSpec>(check.desired)
                && let Err(e) = spec.validate()
            {
                out.push(super::problem(None, ApiError::bad_request(e.to_string())));
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
            let req = request(&spec, Some(&name))?;
            let (_, Json(realm)) = Box::pin(create_realm(
                State(state.clone()),
                principal.clone(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: realm.id,
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
            let req = request(&desired, None)?;
            Box::pin(update_realm(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                super::if_match(live.revision),
                Json(req),
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
            Box::pin(delete_realm(
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
        write_labels_as::<RealmSpec>(cluster, id, revision, labels)
    }

    fn create_carries_labels(&self) -> bool {
        false
    }
}
