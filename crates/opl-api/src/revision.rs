//! 리비전 읽기(설계 §6). 인증된 요청에 헤더 `Orbivisor-Min-Revision`이 있으면 이 노드의 저장소가 그 리비전까지
//! 적용하기를 [`MIN_REVISION_WAIT`]까지 기다린 뒤 처리하고, 넘으면 503 `revision_unavailable`이다. 없으면 지금처럼
//! 로컬 상태로 답한다. 인증된 요청의 응답에는 답할 때 이 노드가 적용한 리비전 `Orbivisor-Revision`을 붙인다. 한
//! 노드에서 쓴 리소스의 리비전을 다른 노드(관찰 노드 등)에 넘기면 자기 쓰기를 읽는다.
//!
//! 기다린 뒤에는 다시 인증한다(`Principal` 추출): 다른 노드에서 막 만든 자격 증명(토큰·사용자·비밀번호)도 그
//! 리비전으로 받고, 기다리는 사이 폐기된 것은 받지 않는다. 인증되지 않은 요청은 이 노드가 있다고 아는 리비전
//! ([`opl_cluster::Cluster::known_revision`])일 때만 기다리고 응답에 리비전을 붙이지 않는다: 없는 리비전으로 연결을
//! 붙잡거나 쓰기 활동을 엿보지 못하게.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::ApiError;
use crate::state::AppState;

/// 요청 헤더: 이 리비전 이후의 상태로 답하라
pub const MIN_REVISION: HeaderName = HeaderName::from_static("orbivisor-min-revision");

/// 응답 헤더: 답한 노드가 적용한 리비전
pub const REVISION: HeaderName = HeaderName::from_static("orbivisor-revision");

/// `Orbivisor-Min-Revision`을 기다리는 최대 시간
pub const MIN_REVISION_WAIT: Duration = Duration::from_secs(5);

/// 잘못된 `Orbivisor-Min-Revision`(400)의 메시지
const BAD_HEADER: &str = "Orbivisor-Min-Revision must be a revision number";

/// 시간 안에 리비전을 따라잡지 못한 503의 코드
const UNAVAILABLE: &str = "revision_unavailable";

/// 요청이 인증됐다는 표시. 층이 요청에 넣고 인증이 세운다.
#[derive(Clone, Default)]
struct Authenticated(Arc<AtomicBool>);

pub(crate) async fn layer(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let authenticated = Authenticated::default();
    req.extensions_mut().insert(authenticated.clone());
    let mut response = next.run(req).await;
    if authenticated.0.load(Ordering::SeqCst) {
        response
            .headers_mut()
            .insert(REVISION, HeaderValue::from(state.cluster.revision()));
    }
    response
}

/// 인증된 요청: 응답에 리비전을 붙이게 표시한다.
pub(crate) fn authenticated(parts: &Parts) {
    if let Some(mark) = parts.extensions.get::<Authenticated>() {
        mark.0.store(true, Ordering::SeqCst);
    }
}

/// `Orbivisor-Min-Revision`의 리비전을 아직 적용하지 않았으면 그때까지 기다린다. 기다렸으면 `true`다(부른 쪽은 다시
/// 인증한다). 인증된 요청(`authenticated`)은 헤더가 잘못됐거나(400) 시간 안에 따라잡지 못하면(503) 오류다. 인증되지
/// 않은 요청은 이 노드가 있다고 아는 리비전일 때만 기다리고, 그 밖에는 기다리지 않는다(`false`, 인증 실패 그대로).
pub(crate) async fn wait_for(
    state: &AppState,
    parts: &Parts,
    authenticated: bool,
) -> Result<bool, ApiError> {
    let revision = match min_revision(parts) {
        Ok(Some(revision)) => revision,
        Ok(None) => return Ok(false),
        Err(e) if authenticated => return Err(e),
        Err(_) => return Ok(false),
    };
    if revision <= state.cluster.revision()
        || (!authenticated && revision > state.cluster.known_revision())
    {
        return Ok(false);
    }
    match wait(state, revision).await {
        Ok(()) => Ok(true),
        Err(e) if authenticated => Err(e),
        Err(_) => Ok(false),
    }
}

/// `Orbivisor-Min-Revision` 때문에 난 오류(400·503)인지
pub(crate) fn rejected(e: &ApiError) -> bool {
    e.code == UNAVAILABLE || e.message == BAD_HEADER
}

fn min_revision(parts: &Parts) -> Result<Option<u64>, ApiError> {
    let Some(value) = parts.headers.get(&MIN_REVISION) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Some)
        .ok_or_else(|| ApiError::bad_request(BAD_HEADER))
}

async fn wait(state: &AppState, revision: u64) -> Result<(), ApiError> {
    state
        .cluster
        .wait_revision(revision, MIN_REVISION_WAIT)
        .await
        .map(|_| ())
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                UNAVAILABLE,
                format!(
                    "this node has not applied revision {revision} yet; try again or ask another node"
                ),
            )
        })
}
