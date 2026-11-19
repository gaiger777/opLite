//! SDN(OVN) E2E의 고리(계획 65 스테이지 2). 시험 빌드(`sdn-it`)에만 있고, `/sdn` API(스테이지 6)가 생기기 전에 제어 소켓으로
//! SDN Task를 만든다 — API가 할 일처럼 `Sdn` 리소스를 만들고 중앙 Task의 자리(`pending`)를 리비전 확인 쓰기로 잡는다.
//!
//! - `POST /v1/test/sdn-task` `{op, node?}`: Task를 만들고 `{id}`(노드는 이름, 없으면 이 노드)
//! - `POST /v1/test/sdn`: `Sdn` 리소스(없으면 null)

use std::sync::{Arc, Mutex, OnceLock, Weak};

use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use opl_cluster::types::Rejection;
use opl_cluster::{Cluster, ClusterError};
use opl_types::node::NodeSpec;
use opl_types::resource::Resource;
use opl_types::sdn::{SDN_NAME, Sdn, SdnPending, SdnSpec, SdnStatus};
use opl_types::task::{Task, TaskOp, TaskSpec};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

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
        .route("/v1/test/sdn-task", post(sdn_task))
        .route("/v1/test/sdn", post(get_sdn))
}

#[derive(Deserialize)]
struct Body {
    op: TaskOp,
    node: Option<String>,
}

async fn get_sdn() -> Result<Json<Value>, Failure> {
    let cluster = member()?;
    let sdn = cluster.get_by_name::<SdnSpec>(SDN_NAME).map_err(bad)?;
    Ok(Json(json!(sdn)))
}

async fn sdn_task(Json(b): Json<Body>) -> Result<Json<Value>, Failure> {
    let cluster = member()?;
    let node = match &b.node {
        Some(name) => cluster.get_by_name::<NodeSpec>(name),
        None => cluster.get::<NodeSpec>(cluster.id().0),
    }
    .map_err(bad)?
    .ok_or_else(|| bad("no such node"))?;
    if !b.op.is_sdn() {
        return Err(bad("not an SDN task"));
    }
    let mut task: Task = Resource::new(
        "",
        TaskSpec {
            op: b.op.clone(),
            target: node.id,
            node: node.id,
        },
    );
    task.name = task.id.to_string();
    let pending = SdnPending {
        op: b.op.clone(),
        node: node.id,
        task: task.id,
    };
    match b.op {
        TaskOp::SdnChassisInstall => {}
        TaskOp::SdnInit { .. } => {
            task.spec.target = claim(&cluster, pending, Some(node.id)).await?;
        }
        _ => task.spec.target = claim(&cluster, pending, None).await?,
    }
    cluster.create(&task).await.map_err(bad)?;
    Ok(Json(json!({ "id": task.id })))
}

/// 자리를 잡는다(첫 세우기면 리소스를 만들거나 첫 세우기 노드를 정한다). 다른 Task가 자리를 가졌으면 409
async fn claim(
    cluster: &Cluster,
    pending: SdnPending,
    init_node: Option<Uuid>,
) -> Result<Uuid, Failure> {
    loop {
        let Some(sdn) = cluster.get_by_name::<SdnSpec>(SDN_NAME).map_err(bad)? else {
            let Some(init) = init_node else {
                return Err(bad("SDN (OVN) is not set up"));
            };
            let mut sdn: Sdn = Resource::new(SDN_NAME, SdnSpec {});
            sdn.status = SdnStatus {
                init_node: Some(init),
                pending: Some(pending.clone()),
                ..SdnStatus::default()
            };
            match cluster.create(&sdn).await {
                Ok(_) => return Ok(sdn.id),
                Err(ClusterError::Rejected(Rejection::NameTaken { .. })) => continue,
                Err(e) => return Err(bad(e)),
            }
        };
        if sdn.status.pending.is_some() {
            return Err((
                StatusCode::CONFLICT,
                "another SDN task holds the slot".into(),
            ));
        }
        let mut status = sdn.status;
        status.pending = Some(pending.clone());
        if init_node.is_some() {
            status.init_node = init_node;
        }
        match cluster
            .update_status_checked::<SdnSpec>(sdn.id, sdn.revision, &status)
            .await
        {
            Ok(_) => return Ok(sdn.id),
            Err(ClusterError::Rejected(Rejection::RevisionMismatch { .. })) => continue,
            Err(e) => return Err(bad(e)),
        }
    }
}
