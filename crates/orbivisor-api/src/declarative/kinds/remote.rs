//! 원격 클러스터(`Remote` — 루트): `url`·`ca_fingerprint`·`token`(비밀 칸). 확인한 CA(`ca_pem`)는 서버가 받아 두는 칸이라
//! 매니페스트에 없다(연결 대상이 그대로면 지금 값). 만들기·바꾸기는 처리기가 원격에 연결해 확인한다. 바꾸기는
//! `PUT /remotes/{key}` 통째(레이블은 따로 쓴다).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use orbivisor_cluster::Cluster;
use orbivisor_types::manifest::{ManifestKind, ManifestState};
use orbivisor_types::remote::RemoteSpec;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, roundtrip, secret,
    typed, without, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::remotes::{RemoteRequest, create_remote, delete_remote, update_remote};
use crate::state::AppState;

const SECRETS: &[&str] = &["token"];

pub(crate) struct RemoteAdapter;

/// 저장 spec → 처리기 본문(토큰은 늘 보낸다 — 지금 값이거나 선언한 값)
fn request(desired: &Value, name: Option<String>) -> Result<RemoteRequest, ApiError> {
    secret::ensure_resolved(desired, SECRETS)?;
    let spec: RemoteSpec = typed(desired).map_err(bad)?;
    Ok(RemoteRequest {
        name,
        url: spec.url,
        ca_fingerprint: spec.ca_fingerprint,
        token: Some(spec.token),
    })
}

impl Adapter for RemoteAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<RemoteSpec>(cluster, ManifestKind::Remote)
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        without(spec, &["ca_pem"])
    }

    fn to_store(
        &self,
        spec: &Value,
        _name: &str,
        _folder: Option<Uuid>,
        _state: Option<ManifestState>,
        _world: &World,
    ) -> Result<Value, String> {
        let mut out = without(spec, &["ca_pem"]);
        secret::refs_to_keep(&mut out, SECRETS);
        if let Some(m) = out.as_object_mut() {
            m.insert("ca_pem".into(), json!(""));
        }
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<RemoteSpec>(store)
    }

    /// 토큰 참조는 지금 토큰, 확인한 CA는 연결 대상(`url`·`ca_fingerprint`)이 그대로일 때 지금 값
    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        secret::restore_kept(desired, live, SECRETS);
        let same = ["url", "ca_fingerprint"]
            .iter()
            .all(|f| desired.get(f) == live.get(f));
        if same && let (Some(m), Some(pem)) = (desired.as_object_mut(), live.get("ca_pem")) {
            m.insert("ca_pem".into(), pem.clone());
        }
    }

    fn updatable(&self) -> Update {
        Update::Whole
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
        check_route(state, principal, ("POST", "/remotes"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PUT", "/remotes/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/remotes/{key}")
    }

    /// 원격에 닿아 보는 확인은 적용 때 처리기가 한다(미리 확인은 쓰지 않으므로 연결하지 않는다).
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = secret::missing(check.desired, SECRETS);
            if check.live.is_none() && check.name == "probe" {
                out.push(Problem::error(
                    "invalid_request",
                    "a remote cannot be named \"probe\"",
                    None,
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
            let req = request(&spec, Some(name))?;
            let (_, Json(remote)) = Box::pin(create_remote(
                State(state.clone()),
                principal.clone(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: remote.id,
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
            Box::pin(update_remote(
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
            Box::pin(delete_remote(
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
        write_labels_as::<RemoteSpec>(cluster, id, revision, labels)
    }

    fn create_carries_labels(&self) -> bool {
        false
    }
}
