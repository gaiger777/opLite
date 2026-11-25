//! 백업 작업(`BackupJob` — 루트): 풀·저장소·원격·VM은 이름이다. 바꾸기는 `PATCH /backup-jobs/{key}`(바뀐 칸만).
//! `vms`(셋 가운데 하나 — 모두·이름·레이블)는 칸마다 얹지 않고 선언으로 통째 바꾸고, `pool`과 `store`는 하나만 남긴다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::backup_job::BackupJobSpec;
use opl_types::manifest::{ManifestKind, ManifestState};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Update, World, bad, check_route, lives_of, refs_of, roundtrip,
    typed, write_labels_as,
};
use crate::auth::Principal;
use crate::backup_jobs::{BackupJobPatch, create_backup_job, delete_backup_job, patch_backup_job};
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::resources::CreateRequest;
use crate::state::AppState;

pub(crate) struct JobAdapter;

/// 바뀐 맨 위 칸의 PATCH 본문. 없어진 칸은 그 칸의 기본(`replicate`·`copy_to`는 `null`, `full_every`는 0)으로,
/// `pool`·`store`가 함께 바뀌면 남은 쪽 하나만 보낸다(처리기는 둘을 함께 받지 않는다).
fn patch_body(desired: &Value, fields: &[String]) -> Value {
    let mut out = Map::new();
    for f in fields {
        let v = desired.get(f).cloned().unwrap_or(Value::Null);
        match f.as_str() {
            "pool" | "store" => {}
            "full_every" if v.is_null() => {
                out.insert(f.clone(), json!(0));
            }
            _ => {
                out.insert(f.clone(), v);
            }
        }
    }
    let pool = desired.get("pool").and_then(Value::as_str).unwrap_or("");
    if fields.iter().any(|f| f == "pool" || f == "store") {
        match desired.get("store").filter(|s| !s.is_null()) {
            Some(store) if pool.is_empty() => {
                out.insert("store".into(), store.clone());
            }
            _ => {
                out.insert("pool".into(), json!(pool));
            }
        }
    }
    Value::Object(out)
}

impl Adapter for JobAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<BackupJobSpec>(cluster, ManifestKind::BackupJob)
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
        Ok(spec.clone())
    }

    fn normalize(&self, store: &Value) -> Result<Value, String> {
        roundtrip::<BackupJobSpec>(store)
    }

    /// 지금 값에 얹은 결과에서 `vms`는 선언 그대로, `pool`·`store`는 선언한 쪽만
    fn keep_server_fields(
        &self,
        _state: &AppState,
        declared: &Value,
        desired: &mut Value,
        _live: &Value,
    ) {
        let Some(obj) = desired.as_object_mut() else {
            return;
        };
        if let Some(vms) = declared.get("vms").filter(|v| !v.is_null()) {
            obj.insert("vms".into(), vms.clone());
        }
        let declared_pool = declared.get("pool").is_some_and(|p| !p.is_null());
        let declared_store = declared.get("store").is_some_and(|s| !s.is_null());
        if declared_pool && !declared_store {
            obj.remove("store");
        } else if declared_store && !declared_pool {
            obj.remove("pool");
        }
    }

    fn updatable(&self) -> Update {
        Update::Fields(&[
            "vms",
            "pool",
            "store",
            "schedule",
            "keep",
            "enabled",
            "replicate",
            "full_every",
            "copy_to",
        ])
    }

    fn keeps_id_on_restore(&self) -> bool {
        true
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        let name = |p: &str| spec.pointer(p).and_then(Value::as_str).map(str::to_string);
        let mut out = refs_of(
            ManifestKind::BackupStore,
            [name("/store"), name("/copy_to/store")]
                .into_iter()
                .flatten(),
        );
        out.extend(refs_of(
            ManifestKind::StoragePool,
            name("/pool").filter(|p| !p.is_empty()),
        ));
        out.extend(refs_of(ManifestKind::Remote, name("/replicate/remote")));
        let vms = spec
            .pointer("/vms/names")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string));
        out.extend(refs_of(ManifestKind::Vm, vms));
        out
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        _desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        check_route(state, principal, ("POST", "/backup-jobs"), None)
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("PATCH", "/backup-jobs/{key}")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/backup-jobs/{key}")
    }

    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            Ok(
                match typed::<BackupJobSpec>(check.desired).map(|s| s.validate()) {
                    Ok(Err(e)) => vec![super::problem(None, ApiError::bad_request(e.to_string()))],
                    _ => Vec::new(),
                },
            )
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
            let spec: BackupJobSpec = typed(&spec).map_err(bad)?;
            let (_, Json(job)) = Box::pin(create_backup_job(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: job.id,
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
            let patch: BackupJobPatch = typed(&patch_body(&desired, &fields)).map_err(bad)?;
            Box::pin(patch_backup_job(
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
            Box::pin(delete_backup_job(
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
        write_labels_as::<BackupJobSpec>(cluster, id, revision, labels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 없어진 칸은 기본값으로, 풀과 저장소는 남은 쪽 하나만
    #[test]
    fn patch_bodies_reset_dropped_fields_and_send_one_of_pool_or_store() {
        let fields = |f: &[&str]| f.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let to_store = json!({ "store": "s3", "schedule": { "at": "01:00" } });
        assert_eq!(
            patch_body(
                &to_store,
                &fields(&["pool", "store", "full_every", "replicate"])
            ),
            json!({ "store": "s3", "full_every": 0, "replicate": null })
        );
        let to_pool = json!({ "pool": "bk" });
        assert_eq!(
            patch_body(&to_pool, &fields(&["pool", "store"])),
            json!({ "pool": "bk" })
        );
        let patch: BackupJobPatch = typed(&patch_body(
            &to_store,
            &fields(&["pool", "store", "copy_to"]),
        ))
        .unwrap();
        assert_eq!(patch.store.as_deref(), Some("s3"));
        assert_eq!(patch.copy_to, Some(None));
    }
}
