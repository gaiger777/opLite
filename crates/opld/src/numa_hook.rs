//! NUMA E2E의 고리(계획 64). 시험 빌드(`numa-it`)에만 있고 제어 소켓에 API가 아직 받지 않는 쓰기 둘을 더한다(스테이지 5
//! 전에는 API가 새 칸을 막는다 — 계획 63처럼 클러스터에 바로 쓴다). Raft 받아들이기 검사는 그대로 걸린다.
//!
//! - `POST /v1/test/vm-spec` `{vm, patch}`: VM 스펙 JSON에 `patch`를 덮는다(`cpu`는 칸마다).
//! - `POST /v1/test/host-resources` `{resources}`: 이 노드의 `host_resources`(null이면 지움)

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_types::node::NodeSpec;
use opl_types::vm::VmSpec;
use serde::Deserialize;
use serde_json::Value;

static MEMBER: OnceLock<Mutex<Option<Weak<Cluster>>>> = OnceLock::new();

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소를 알린다.
pub fn set_member(cluster: Weak<Cluster>) {
    *MEMBER.get_or_init(Default::default).lock().unwrap() = Some(cluster);
}

fn member() -> Result<Arc<Cluster>, (StatusCode, String)> {
    let gone = || (StatusCode::NOT_FOUND, "not a member".to_string());
    let m = MEMBER.get().ok_or_else(gone)?.lock().unwrap().clone();
    m.ok_or_else(gone)?.upgrade().ok_or_else(gone)
}

fn bad(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/vm-spec", post(vm_spec))
        .route("/v1/test/host-resources", post(host_resources))
}

#[derive(Deserialize)]
struct Patch {
    vm: String,
    patch: serde_json::Map<String, Value>,
}

async fn vm_spec(Json(b): Json<Patch>) -> Result<Json<Value>, (StatusCode, String)> {
    let cluster = member()?;
    let vm = cluster
        .get_by_name::<VmSpec>(&b.vm)
        .map_err(bad)?
        .ok_or_else(|| bad(format!("no VM {:?}", b.vm)))?;
    let mut spec = serde_json::to_value(&vm.spec).map_err(bad)?;
    for (k, v) in b.patch {
        match (k.as_str(), v) {
            ("cpu", Value::Object(cpu)) => {
                for (ck, cv) in cpu {
                    spec["cpu"][ck] = cv;
                }
            }
            (_, v) => spec[k] = v,
        }
    }
    let spec: VmSpec = serde_json::from_value(spec).map_err(bad)?;
    cluster
        .update_spec(vm.id, Some(vm.revision), vm.labels, &spec)
        .await
        .map_err(bad)?;
    Ok(Json(Value::Null))
}

#[derive(Deserialize)]
struct Resources {
    resources: Option<opl_types::numa::HostResources>,
}

async fn host_resources(Json(b): Json<Resources>) -> Result<Json<Value>, (StatusCode, String)> {
    let cluster = member()?;
    let me = cluster
        .get::<NodeSpec>(cluster.id().0)
        .map_err(bad)?
        .ok_or_else(|| bad("this node is not registered"))?;
    let mut spec = me.spec.clone();
    spec.host_resources = b.resources;
    cluster
        .update_spec(me.id, Some(me.revision), me.labels, &spec)
        .await
        .map_err(bad)?;
    Ok(Json(Value::Null))
}
