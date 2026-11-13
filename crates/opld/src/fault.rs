//! 망 끊기 시험의 고리(`ha-it` 빌드에만). 제어 소켓에 경로 둘을 더한다.
//!
//! - `POST /v1/test/isolate`: 이 노드의 노드 간 통신(보내기·받기)을 모두 끊는다(opl-peer `fault-it`). API와
//!   제어 소켓은 그대로다.
//! - `POST /v1/test/heal`: 다시 잇는다.

use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/v1/test/isolate", post(isolate))
        .route("/v1/test/heal", post(heal))
}

async fn isolate() -> StatusCode {
    tracing::warn!("test hook: isolating this node from its peers");
    opl_peer::fault::isolate();
    StatusCode::NO_CONTENT
}

async fn heal() -> StatusCode {
    tracing::warn!("test hook: reconnecting this node to its peers");
    opl_peer::fault::heal();
    StatusCode::NO_CONTENT
}
