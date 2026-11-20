//! SDN 라우팅 E2E의 고리(계획 66 스테이지 2). 시험 빌드(`sdn-it`)에만 있고, `PUT /nodes/{key}`의 `spec.sdn`(스테이지 5까지
//! API가 막음)이 생기기 전에 제어 소켓으로 이 노드의 SDN 설정을 쓴다. 스테이지 5가 지운다.
//!
//! - `POST /v1/test/node-sdn` `{sdn}`: 이 노드의 `NodeSpec.sdn`(physnet·게이트웨이, null이면 지움)을 검증해 쓴다.

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::Cluster;
use opl_types::node::NodeSpec;
use opl_types::sdn::NodeSdnConfig;
use serde::Deserialize;

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
    Router::new().route("/v1/test/node-sdn", post(node_sdn))
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
