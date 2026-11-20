//! SDN 서비스 E2E의 고리(계획 67 스테이지 2). 시험 빌드(`sdn-it`)에만 있고, 부하 분산 API와 NIC `qos`(스테이지 5까지
//! API가 막음)가 생기기 전에 제어 소켓으로 클러스터에 바로 쓴다. 스테이지 5가 지운다(계획 66의 같은 고리처럼).
//!
//! - `POST /v1/test/put` `{kind, name, spec}`: `load-balancer`는 스펙 검증만 하고 이름으로 만들거나 바꾼다(`spec`이
//!   null이면 지움). `vm`은 있는 VM의 스펙을 통째로 바꾼다(NIC `qos` 따위 — 검증만, 만들기·지우기는 API로). 답은
//!   리소스 id다. API의 검사(IPAM·권한·기능·VIP 고르기)는 스테이지 5에서 생긴다.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_types::load_balancer::LoadBalancerSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::vm::VmSpec;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

static MEMBER: OnceLock<Mutex<Option<Weak<Cluster>>>> = OnceLock::new();

/// 멤버가 되면(다시 시작 포함) 그 클러스터 저장소를 알린다.
pub fn set_member(cluster: Weak<Cluster>) {
    *MEMBER.get_or_init(Default::default).lock().unwrap() = Some(cluster);
}

type Failure = (StatusCode, String);

fn member() -> Result<Arc<Cluster>, Failure> {
    let gone = || (StatusCode::NOT_FOUND, "not a member".to_string());
    let m = MEMBER.get().ok_or_else(gone)?.lock().unwrap().clone();
    m.ok_or_else(gone)?.upgrade().ok_or_else(gone)
}

fn bad(e: impl std::fmt::Display) -> Failure {
    (StatusCode::BAD_REQUEST, e.to_string())
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route("/v1/test/put", post(put))
}

#[derive(Deserialize)]
struct Put {
    kind: String,
    name: String,
    spec: Option<Value>,
}

async fn put(Json(b): Json<Put>) -> Result<Json<Option<uuid::Uuid>>, Failure> {
    let cluster = member()?;
    let id = match b.kind.as_str() {
        "load-balancer" => {
            let spec: Option<LoadBalancerSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
        }
        "vm" => {
            let spec: VmSpec = parse(b.spec)?.ok_or_else(|| bad("a vm needs a spec"))?;
            spec.validate().map_err(bad)?;
            let current = cluster
                .get_by_name::<VmSpec>(&b.name)
                .map_err(bad)?
                .ok_or_else(|| bad(format!("no VM {:?}", b.name)))?;
            put_one(&cluster, &b.name, Some(spec)).await?;
            Some(current.id)
        }
        other => return Err(bad(format!("unknown kind {other}"))),
    };
    Ok(Json(id))
}

fn parse<S: DeserializeOwned>(spec: Option<Value>) -> Result<Option<S>, Failure> {
    spec.map(serde_json::from_value).transpose().map_err(bad)
}

/// 이름 `name`의 리소스를 `spec`으로 만들거나 바꾸고(None이면 지우고) id를 돌려준다.
async fn put_one<S: ResourceSpec>(
    cluster: &Cluster,
    name: &str,
    spec: Option<S>,
) -> Result<Option<uuid::Uuid>, Failure> {
    let current = cluster.get_by_name::<S>(name).map_err(bad)?;
    match (current, spec) {
        (Some(r), Some(spec)) => {
            cluster
                .update_spec(r.id, Some(r.revision), r.labels, &spec)
                .await
                .map_err(bad)?;
            Ok(Some(r.id))
        }
        (None, Some(spec)) => {
            let r = Resource::new(name, spec);
            cluster.create(&r).await.map_err(bad)?;
            Ok(Some(r.id))
        }
        (Some(r), None) => {
            cluster
                .delete::<S>(r.id, Some(r.revision))
                .await
                .map_err(bad)?;
            Ok(None)
        }
        (None, None) => Ok(None),
    }
}
