//! 볼륨(`Volume` — 빈 디스크): 풀을 이름으로 가리킨다. 바꿀 수 있는 것은 크기 늘리기뿐(`POST /volumes/{key}/resize` —
//! 줄이기·풀·형식은 막힘). 만들기·지우기·늘리기는 Task를 만들고 적용은 그 Task를 기다린다.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use opl_cluster::Cluster;
use opl_types::authz::Privilege;
use opl_types::manifest::{ManifestKind, ManifestState};
use opl_types::storage::{StoragePoolSpec, VolumeSpec};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{
    Adapter, Check, Done, Fut, Live, Planned, Update, World, bad, lives_of, problem, roundtrip,
    typed, write_labels_as,
};
use crate::auth::Principal;
use crate::declarative::Problem;
use crate::error::{ApiError, Json};
use crate::resources::CreateRequest;
use crate::state::AppState;
use crate::volumes::{
    ResizeRequest, check_new_volume, create_volume, default_format, delete_volume, find_pool,
    resize_volume,
};

pub(crate) struct VolumeAdapter;

impl Adapter for VolumeAdapter {
    fn lives(&self, cluster: &Cluster) -> Result<Vec<Live>, ApiError> {
        lives_of::<VolumeSpec>(cluster, ManifestKind::Volume)
    }

    /// 연결 복제의 기반(`base`)은 배포 Task만 정한다 — 내보내지 않는다.
    fn to_manifest(&self, spec: &Value, _world: &World) -> Value {
        super::without(spec, &["base"])
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
        roundtrip::<VolumeSpec>(store)
    }

    fn keep_server_fields(
        &self,
        _state: &AppState,
        _declared: &Value,
        desired: &mut Value,
        live: &Value,
    ) {
        if let (Some(obj), Some(base)) = (desired.as_object_mut(), live.get("base")) {
            obj.insert("base".into(), base.clone());
        }
    }

    /// 형식을 적지 않으면 풀의 기본(블록 풀은 raw, 그 밖은 qcow2 — `POST /volumes`를 부르는 웹과 같다)
    fn create_defaults(
        &self,
        state: &AppState,
        _world: &World,
        planned: &Planned,
        desired: &mut Value,
    ) -> Result<(), ApiError> {
        if desired.get("format").is_some() {
            return Ok(());
        }
        let Some(pool) = desired
            .get("pool")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            return Ok(());
        };
        let format = match find_pool(state, &pool) {
            Ok(p) => serde_json::to_value(default_format(&p)).expect("format"),
            Err(_) => {
                // 같은 계획이 만들 풀: 그 백엔드로 정한다.
                let backend = planned
                    .docs
                    .get(&(ManifestKind::StoragePool, pool))
                    .and_then(|d| d.spec.pointer("/backend/kind"))
                    .and_then(Value::as_str);
                match backend {
                    Some("dir" | "nfs") | None => json!("qcow2"),
                    Some(_) => json!("raw"),
                }
            }
        };
        if let Some(obj) = desired.as_object_mut() {
            obj.insert("format".into(), format);
        }
        Ok(())
    }

    fn updatable(&self) -> Update {
        Update::Fields(&["size_bytes"])
    }

    fn refs(&self, spec: &Value) -> Vec<(ManifestKind, String)> {
        spec.get("pool")
            .and_then(Value::as_str)
            .map(|p| vec![(ManifestKind::StoragePool, p.to_string())])
            .unwrap_or_default()
    }

    fn may_create(
        &self,
        state: &AppState,
        principal: &Principal,
        desired: &Value,
        _folder: Option<Uuid>,
    ) -> Result<(), ApiError> {
        // 같은 계획이 만들 풀이면 적용 때 처리기가 본다.
        let Some(pool) = desired.get("pool").and_then(Value::as_str) else {
            return Ok(());
        };
        match state.cluster.get_by_name::<StoragePoolSpec>(pool)? {
            Some(pool) => state
                .authz
                .require(principal, Privilege::StorageAllocate, &pool),
            None => Ok(()),
        }
    }

    fn update_route(&self) -> (&'static str, &'static str) {
        ("POST", "/volumes/{key}/resize")
    }

    fn delete_route(&self) -> (&'static str, &'static str) {
        ("DELETE", "/volumes/{key}")
    }

    /// 만들기는 `POST /volumes`의 검사(풀이 있을 때), 바꾸기는 줄이기를 막는다.
    fn precheck<'a>(&'a self, check: Check<'a>) -> Fut<'a, Vec<Problem>> {
        Box::pin(async move {
            let mut out = Vec::new();
            let Ok(spec) = typed::<VolumeSpec>(check.desired) else {
                return Ok(out);
            };
            match check.live {
                None => {
                    let planned_pool = check
                        .planned
                        .docs
                        .contains_key(&(ManifestKind::StoragePool, spec.pool.clone()));
                    if !planned_pool {
                        let req = CreateRequest {
                            name: check.name.to_string(),
                            labels: check.labels.clone(),
                            spec,
                        };
                        if let Err(e) = check_new_volume(check.state, check.principal, &req) {
                            out.push(problem(None, e));
                        }
                    }
                }
                Some(live) => {
                    let now = live
                        .spec
                        .get("size_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if spec.size_bytes < now {
                        out.push(Problem::error(
                            "immutable",
                            format!(
                                "volume {:?} cannot shrink from {now} to {} bytes; make a new volume",
                                check.name, spec.size_bytes
                            ),
                            Some("size_bytes".into()),
                        ));
                    }
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
            let spec: VolumeSpec = typed(&spec).map_err(bad)?;
            let (_, Json(task)) = Box::pin(create_volume(
                State(state.clone()),
                principal.clone(),
                Json(CreateRequest { name, labels, spec }),
            ))
            .await?;
            Ok(Done {
                id: task.spec.target,
                task: Some(task.id),
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
            let size_bytes = desired
                .get("size_bytes")
                .and_then(Value::as_u64)
                .ok_or_else(|| ApiError::bad_request("spec.size_bytes is required"))?;
            let (_, Json(task)) = Box::pin(resize_volume(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
                Json(ResizeRequest { size_bytes }),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: Some(task.id),
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
            let (_, Json(task)) = Box::pin(delete_volume(
                State(state.clone()),
                principal.clone(),
                Path(live.id.to_string()),
            ))
            .await?;
            Ok(Done {
                id: live.id,
                task: Some(task.id),
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
        write_labels_as::<VolumeSpec>(cluster, id, revision, labels)
    }
}
