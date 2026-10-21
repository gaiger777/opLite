//! opLite REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod auth;
pub mod error;
pub mod resources;
pub mod scheduler;
mod state;
mod tasks;
pub mod users;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use state::AppState;
pub use tasks::{TaskRunner, TaskRunnerConfig};

use crate::resources as r;

#[derive(OpenApi)]
#[openapi(
    info(title = "opLite API", description = "opLite REST API (`/api/v1`)"),
    modifiers(&Security),
    security(("token" = []), ("session" = [])),
)]
struct ApiDoc;

/// API 토큰(`Authorization: Bearer opl_...`)과 세션 쿠키
struct Security;

impl Modify for Security {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "token",
            SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
        );
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(auth::SESSION_COOKIE))),
        );
    }
}

fn api() -> OpenApiRouter<Arc<AppState>> {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(users::login))
        .routes(routes!(users::logout))
        .routes(routes!(users::whoami))
        .routes(routes!(users::list_users, users::create_user))
        .routes(routes!(users::update_user, users::delete_user))
        .routes(routes!(users::list_tokens, users::create_token))
        .routes(routes!(users::delete_token))
        .routes(routes!(r::list_nodes))
        .routes(routes!(r::get_node, r::update_node))
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::delete_pool))
        .routes(routes!(r::list_tasks))
        .routes(routes!(r::get_task))
}

/// API 문서. 라우터와 같은 정의에서 만들므로 경로가 어긋나지 않는다.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api().into_openapi()
}

/// `/api/v1` 아래의 API와 인증 없이 읽는 `/api/v1/openapi.json`
pub fn router(state: Arc<AppState>) -> Router {
    let (api, doc) = api().split_for_parts();
    let doc = Arc::new(doc);
    let api = api.route(
        "/openapi.json",
        get(move || {
            let doc = doc.clone();
            async move { axum::Json(doc.as_ref().clone()) }
        }),
    );
    Router::new().nest("/api/v1", api).with_state(state)
}
