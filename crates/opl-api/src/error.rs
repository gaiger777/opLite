use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use opl_cluster::ClusterError;
use opl_cluster::types::Rejection;
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

/// API 오류. 응답 본문은 `{ "code": "...", "message": "...", "details": {...} }`이다(설계 4절).
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub details: Option<Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "authentication required",
        )
    }

    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "permission denied")
    }

    /// 자세한 내용은 로그에만 남기고 클라이언트에는 드러내지 않는다.
    pub fn internal(e: impl std::fmt::Display) -> Self {
        tracing::error!(error = %e, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        )
    }
}

/// 오류 응답 본문
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// 기계가 읽는 오류 종류. 예: `not_found`, `revision_mismatch`, `cluster_no_quorum`
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code.to_string(),
            message: self.message,
            details: self.details,
        };
        (self.status, axum::Json(body)).into_response()
    }
}

impl From<ClusterError> for ApiError {
    fn from(e: ClusterError) -> Self {
        match e {
            ClusterError::Rejected(r) => {
                let message = r.to_string();
                match r {
                    Rejection::AlreadyExists { .. } => {
                        ApiError::conflict("already_exists", message)
                    }
                    Rejection::NameTaken { .. } => ApiError::conflict("name_taken", message),
                    Rejection::NotFound { .. } => ApiError::not_found(message),
                    Rejection::RevisionMismatch { .. } => ApiError::new(
                        StatusCode::PRECONDITION_FAILED,
                        "revision_mismatch",
                        message,
                    ),
                    Rejection::InUse { field, .. } => ApiError::conflict(
                        if field == "mac" {
                            "mac_in_use"
                        } else {
                            "volume_in_use"
                        },
                        message,
                    ),
                }
            }
            ClusterError::Timeout | ClusterError::Unreachable(_) => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cluster_no_quorum",
                "the cluster has no quorum; the change may still be applied later",
            ),
            ClusterError::NotLeader {
                leader_id,
                leader_addr,
            } => {
                let mut e = ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "not_leader",
                    "this node is not the cluster leader",
                );
                e.details = Some(serde_json::json!({
                    "leader_id": leader_id.map(|id| id.0),
                    "leader_addr": leader_addr,
                }));
                e
            }
            other => ApiError::internal(other),
        }
    }
}

impl From<JsonRejection> for ApiError {
    fn from(e: JsonRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

impl From<WebSocketUpgradeRejection> for ApiError {
    fn from(e: WebSocketUpgradeRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(e: QueryRejection) -> Self {
        ApiError::bad_request(e.body_text())
    }
}

/// 본문 오류도 API 오류 형식으로 돌려주는 JSON 추출기·응답.
#[derive(FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// 쿼리 오류도 API 오류 형식으로 돌려주는 추출기.
#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_used_by_another_vm_are_conflicts() {
        let e = ApiError::from(ClusterError::Rejected(Rejection::InUse {
            kind: opl_types::resource::Kind::Vm,
            field: "mac".into(),
            value: "52:54:00:00:00:01".into(),
            holder: uuid::Uuid::nil(),
        }));
        assert_eq!(e.status, StatusCode::CONFLICT);
        assert_eq!(e.code, "mac_in_use");
        assert!(e.message.contains("52:54:00:00:00:01"));
    }

    #[test]
    fn internal_error_hides_details() {
        let e = ApiError::internal("disk /var/lib/opl is full");
        assert_eq!(e.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(e.message, "internal error");
    }
}
