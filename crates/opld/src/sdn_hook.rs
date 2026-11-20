//! SDN 라우팅 E2E의 고리(계획 66 스테이지 2). 시험 빌드(`sdn-it`)에만 있고, `PUT /nodes/{key}`의 `spec.sdn`(스테이지 5까지
//! API가 막음)이 생기기 전에 제어 소켓으로 이 노드의 SDN 설정을 쓴다. 스테이지 5가 지운다.
//!
//! - `POST /v1/test/node-sdn` `{sdn}`: 이 노드의 `NodeSpec.sdn`(physnet·게이트웨이, null이면 지움)을 검증해 쓴다.
//! - `POST /v1/test/put` `{kind, name, spec}`(스테이지 3): 공급자 네트워크·라우터·유동 IP(`network`·`router`·`floating-ip`)를
//!   스펙 검증만 하고 이름으로 만들거나 바꾼다(`spec`이 null이면 지움). 답은 리소스 id다. API의 검사(IPAM·권한·기능)는
//!   스테이지 5에서 생긴다.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_types::floating_ip::FloatingIpSpec;
use opl_types::network::NetworkSpec;
use opl_types::node::NodeSpec;
use opl_types::resource::{Resource, ResourceSpec};
use opl_types::router::RouterSpec;
use opl_types::sdn::NodeSdnConfig;
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
    Router::new()
        .route("/v1/test/node-sdn", post(node_sdn))
        .route("/v1/test/put", post(put))
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
        "network" => {
            let spec: Option<NetworkSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
        }
        "router" => {
            let spec: Option<RouterSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
        }
        "floating-ip" => {
            let spec: Option<FloatingIpSpec> = parse(b.spec)?;
            if let Some(s) = &spec {
                s.validate().map_err(bad)?;
            }
            put_one(&cluster, &b.name, spec).await?
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

#[derive(Deserialize)]
struct Body {
    sdn: Option<NodeSdnConfig>,
}

async fn node_sdn(Json(b): Json<Body>) -> Result<Json<()>, Failure> {
    let cluster = member()?;
    if let Some(sdn) = &b.sdn {
        sdn.validate().map_err(bad)?;
    }
    let me = cluster
        .get::<NodeSpec>(cluster.id().0)
        .map_err(bad)?
        .ok_or_else(|| bad("no such node"))?;
    let mut spec = me.spec;
    spec.sdn = b.sdn;
    cluster
        .update_spec(me.id, Some(me.revision), me.labels, &spec)
        .await
        .map_err(bad)?;
    Ok(Json(()))
}
