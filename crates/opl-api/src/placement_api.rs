//! 최적 배치 API(설계 §8.3): 리더가 아는 노드 부하와 균형 조정기의 결과.

use std::sync::Arc;

use axum::extract::State;
use opl_types::node::NodeSpec;
use opl_types::placement::PlacementStatus;
use opl_types::resource::ResourceSpec;

use crate::auth::{Action, Principal};
use crate::error::{ApiError, Json};
use crate::state::AppState;

/// 최적 배치 상태: 설정, 노드마다의 부하(리더가 리스로 받은 요약), 균형 조정기의 마지막 결과. 로그인한 누구나 읽는다.
/// 리더가 아닌 노드는 리더에 묻고, 묻지 못하면 503 `placement_unavailable`이다.
#[utoipa::path(get, path = "/placement", tag = "placement",
    responses((status = 200, body = PlacementStatus), (status = "4XX", body = crate::error::ErrorBody),
        (status = 503, body = crate::error::ErrorBody)))]
pub async fn get_placement(
    State(state): State<Arc<AppState>>,
    principal: Principal,
) -> Result<Json<PlacementStatus>, ApiError> {
    state.authorize(&principal, Action::Read, NodeSpec::KIND)?;
    Ok(Json(crate::score::status(&state).await?))
}
