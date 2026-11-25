//! 백업 저장소(`BackupStore` — 루트, 데이터 종류): 백엔드(풀·S3)·읽기 전용·GC·검증·동시 요청 수·암호화. 주 키
//! (`encryption.key`, base64)와 S3 비밀 키는 비밀 칸이다. 만들기는 `POST /backup-stores`(주 키는 복구 키로 바꿔 넘기고,
//! 초기화 Task를 기다린다). 바꿀 수 있는 것은 GC·검증·동시 요청 수와 S3 자격 증명·CA뿐이다(`PATCH`) — 백엔드의 다른 칸·
//! 암호화·읽기 전용은 막힘(`immutable`).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_store::crypto::{MasterKey, StoreKeys};
use opl_types::backup_store::BackupStoreSpec;
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, problem, refs_of,
    roundtrip, secret, typed, write_labels_as,
};
use crate::audit::AuditNote;
use crate::auth::Principal;
use crate::backup_stores::{
    CreateStoreRequest, StorePatch, create_store, delete_store, patch_store,
};
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::state::AppState;

const SECRETS: &[&str] = &["encryption.key", "backend.s3.secret_access_key"];
/// PATCH가 바꾸는 S3 칸
const S3_PATCHABLE: [&str; 3] = ["access_key_id", "secret_access_key", "ca_pem"];

pub(crate) struct StoreAdapter;

/// 주 키(base64)의 지문
fn fingerprint_of(key: &str) -> Option<String> {
    let mk = MasterKey::from_base64(key).ok()?;
    Some(StoreKeys::new(&mk).fingerprint().to_string())
}

/// 백엔드에서 PATCH로 바꿀 수 있는 S3 칸을 뺀 것(바꿀 수 없는 칸이 다른지 견준다)
fn fixed_backend(spec: &Value) -> Value {
    let mut b = spec.get("backend").cloned().unwrap_or(Value::Null);
    if let Some(s3) = b.get_mut("s3").and_then(Value::as_object_mut) {
        for f in S3_PATCHABLE {
            s3.remove(f);
        }
    }
    b
}

/// 저장 spec → 만들기 본문(주 키는 복구 키로)
fn create_request(
    name: String,
    labels: BTreeMap<String, String>,
    spec: &Value,
) -> Result<CreateStoreRequest, ApiError> {
    secret::ensure_resolved(spec, SECRETS)?;
    let mut body = json!({ "name": name, "labels": labels, "spec": {} });
    for f in ["backend", "read_only", "gc", "verify", "max_streams"] {
        if let Some(v) = spec.get(f).filter(|v| !v.is_null()) {
            body["spec"][f] = v.clone();
        }
    }
    if let Some(key) = spec.pointer("/encryption/key").and_then(Value::as_str) {
        let mk = MasterKey::from_base64(key).map_err(|_| {
            ApiError::bad_request("encryption.key is not a base64 32-byte store key")
        })?;
        body["encryption"] = json!({ "recovery_key": mk.recovery_key() });
    }
    typed(&body).map_err(bad)
}

/// 바뀐 맨 위 칸의 PATCH 본문(백엔드는 S3 자격 증명·CA만)
fn patch_body(desired: &Value, live: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        match f.as_str() {
            "gc" | "max_streams" => {
                if let Some(v) = desired.get(f) {
                    out.insert(f.clone(), v.clone());
                }
            }
            "verify" => {
                out.insert(f.clone(), desired.get(f).cloned().unwrap_or(Value::Null));
            }
            "backend" => {
                let (Some(new), Some(old)) =
                    (desired.pointer("/backend/s3"), live.pointer("/backend/s3"))
                else {
                    continue;
                };
                let creds = ["access_key_id", "secret_access_key"];
                if creds.iter().any(|c| new.get(c) != old.get(c)) {
                    out.insert(
                        "s3_credentials".into(),
                        json!({ "access_key_id": new["access_key_id"],
                                "secret_access_key": new["secret_access_key"] }),
                    );
                }
                if new.get("ca_pem") != old.get("ca_pem") {
                    out.insert(
                        "ca_pem".into(),
                        new.get("ca_pem").cloned().unwrap_or(Value::Null),
                    );
                }
            }
            _ => {}
        }
    }
    Value::Object(out)
}

impl Adapter for StoreAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<BackupStoreSpec>(cluster, ManifestKind::BackupStore)
    }

    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        spec.clone()
    }

    /// 참조는 그대로 둠 표시로, 지문 없이 준 평문 주 키에는 지문을 채운다.
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
        if let Some(enc) = out.get_mut("encryption").and_then(Value::as_object_mut)
            && !enc.contains_key("fingerprint")
            && let Some(fp) = enc
                .get("key")
                .and_then(Value::as_str)
                .and_then(fingerprint_of)
        {
            enc.insert("fingerprint".into(), json!(fp));
        }
        Ok(out)
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<BackupStoreSpec>(store)
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
        Update::Fields(&["gc", "verify", "max_streams", "backend"])
    }

    fn secret_paths(&self) -> &'static [&'static str] {
        SECRETS
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        refs_of(
            ManifestKind::StoragePool,
            spec.pointer("/backend/pool")
                .and_then(Value::as_str)
                .map(str::to_string),
        )
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/backup-stores"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/backup-stores/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/backup-stores/{key}")
    }

    /// 비밀 참조·주 키 꼴·지문, 백엔드의 바꿀 수 없는 칸, spec 검증
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let d = check.desired;
            let mut out = secret::missing(d, SECRETS);
            if !out.is_empty() {
                return Ok(out);
            }
            match check.live {
                None => {
                    if let Some(key) = d.pointer("/encryption/key").and_then(Value::as_str) {
                        match fingerprint_of(key) {
                            None => out.push(Problem::error(
                                "invalid_backup_store",
                                "encryption.key is not a base64 32-byte store key",
                                Some("encryption.key".into()),
                            )),
                            Some(fp)
                                if d.pointer("/encryption/fingerprint").and_then(Value::as_str)
                                    != Some(fp.as_str()) =>
                            {
                                out.push(Problem::error(
                                    "store_key_mismatch",
                                    format!(
                                        "encryption.fingerprint does not match the key (it is {fp})"
                                    ),
                                    Some("encryption.fingerprint".into()),
                                ));
                            }
                            Some(_) => {}
                        }
                    }
                }
                Some(live) => {
                    if fixed_backend(d) != fixed_backend(&live.spec) {
                        out.push(Problem::error(
                            "immutable",
                            format!(
                                "BackupStore {:?}: only the S3 credentials and ca_pem of the backend can change; delete and recreate it by hand",
                                check.name
                            ),
                            Some("backend".into()),
                        ));
                    }
                }
            }
            if let Ok(Err(e)) = typed::<BackupStoreSpec>(d).map(|s| s.validate()) {
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
            let req = create_request(name, labels, &spec)?;
            let (_, Json(created)) = Box::pin(create_store(
                State(state.clone()),
                principal.clone(),
                AuditNote::detached(),
                Json(req),
            ))
            .await?;
            Ok(Done {
                id: created.store.id,
                task: Some(created.task.id),
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
            secret::ensure_resolved(&desired, SECRETS)?;
            let patch: StorePatch =
                typed(&patch_body(&desired, &live.spec, &fields)).map_err(bad)?;
            Box::pin(patch_store(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
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
            Box::pin(delete_store(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
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
        write_labels_as::<BackupStoreSpec>(cluster, id, revision, labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 주 키는 복구 키로 넘기고, 자격 증명만 바뀐 백엔드는 `s3_credentials` 하나다.
    #[test]
    fn keys_become_recovery_keys_and_credentials_patch_alone() {
        let mk = MasterKey::generate();
        let spec = json!({ "backend": { "pool": "bk" }, "max_streams": 4,
                           "encryption": { "key": mk.to_base64(), "fingerprint": "x" } });
        let req = create_request("s".into(), BTreeMap::new(), &spec).unwrap();
        assert_eq!(
            req.encryption.and_then(|e| e.recovery_key),
            Some(mk.recovery_key())
        );
        let old = json!({ "backend": { "s3": { "endpoint": "e", "bucket": "b",
            "access_key_id": "A", "secret_access_key": "S" } } });
        let mut new = old.clone();
        new["backend"]["s3"]["secret_access_key"] = json!("S2");
        assert_eq!(fixed_backend(&old), fixed_backend(&new));
        assert_eq!(
            patch_body(&new, &old, &["backend".into()]),
            json!({ "s3_credentials": { "access_key_id": "A", "secret_access_key": "S2" } })
        );
        new["backend"]["s3"]["bucket"] = json!("other");
        assert_ne!(fixed_backend(&old), fixed_backend(&new));
    }
}
