//! Orbivisor REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

mod api_cert;
pub mod audit;
pub mod auth;
mod backup_jobs;
mod backup_receive;
mod backups;
pub mod cluster;
mod console;
mod drain;
pub mod error;
mod fan_out;
mod firmware;
mod ha;
mod ha_groups;
pub mod health;
mod incoming;
mod janitor;
pub mod ldap;
pub mod limit;
mod metrics;
mod migration_disks;
pub mod oidc;
mod oidc_login;
pub mod placement;
mod placement_rules;
pub mod proxy;
pub mod realms;
pub mod remote_client;
mod remote_proxy;
mod remotes;
pub mod resources;
pub mod revision;
mod schedule;
pub mod scheduler;
mod security_groups;
mod settings;
mod snapshots;
mod state;
mod subscription;
mod tasks;
mod telemetry;
mod updates;
pub mod users;
pub mod vip;
pub mod vm_imports;
mod vms;
mod volumes;
mod watch;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use api_cert::{ApiCertAgent, ApiCertError};
pub use audit::{AuditConfig, AuditLog};
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use backup_jobs::{BackupJobScheduler, BackupJobSchedulerConfig};
pub use drain::start_drain;
pub use firmware::UefiVarsFetcher;
pub use ha::{HaManager, HaManagerConfig, RecoveryWriter};
pub use health::RuntimeProbe;
pub use incoming::{IncomingConfig, IncomingMigrations};
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
pub use remotes::{RemotePoller, RemotePollerConfig};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
pub use state::AppState;
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use telemetry::{TelemetryConfig, TelemetryReporter};
pub use updates::{AUTH_FILE, KEYRING, UpdateAgent, Updates, UpdatesConfig};
pub use vip::{VipAgent, VipAgentConfig, VipManager, VipManagerConfig};

use crate::resources as r;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Orbivisor API",
        description = "Orbivisor REST API (`/api/v1`). Every authenticated response carries `Orbivisor-Revision`, \
            the revision the answering node had applied when it answered. An authenticated request with \
            `Orbivisor-Min-Revision: <revision>` is answered once the node has applied that revision (503 \
            `revision_unavailable` after 5 seconds); pass the `revision` of a resource written elsewhere to read it."
    ),
    servers((url = "/api/v1")),
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
        .routes(routes!(realms::list_login_realms))
        .routes(routes!(oidc_login::oidc_start))
        .routes(routes!(oidc_login::oidc_callback))
        .routes(routes!(users::list_users, users::create_user))
        .routes(routes!(users::update_user, users::delete_user))
        .routes(routes!(users::list_tokens, users::create_token))
        .routes(routes!(users::delete_token))
        .routes(routes!(realms::list_realms, realms::create_realm))
        .routes(routes!(
            realms::get_realm,
            realms::update_realm,
            realms::delete_realm
        ))
        .routes(routes!(realms::test_realm))
        .routes(routes!(remotes::list_remotes, remotes::create_remote))
        .routes(routes!(remotes::probe_remote))
        .routes(routes!(
            remotes::get_remote,
            remotes::update_remote,
            remotes::delete_remote
        ))
        .routes(routes!(remotes::refresh_remote))
        // 모든 메서드를 받는 중계는 API 문서에 넣지 않는다.
        .route(
            "/remotes/{key}/api/{*path}",
            axum::routing::any(remote_proxy::proxy),
        )
        .routes(routes!(r::list_nodes))
        .routes(routes!(r::get_node, r::update_node, cluster::delete_node))
        .routes(routes!(updates::refresh_updates))
        .routes(routes!(updates::upgrade_node))
        .routes(routes!(drain::drain_node))
        .routes(routes!(drain::unfreeze_node))
        .routes(routes!(metrics::get_node_metrics))
        .routes(routes!(metrics::list_node_metrics))
        .routes(routes!(cluster::get_cluster))
        .routes(routes!(cluster::get_ca))
        .routes(routes!(cluster::get_version))
        .routes(routes!(health::get_health))
        .routes(routes!(cluster::list_api_endpoints))
        .routes(routes!(cluster::create_join_token))
        .routes(routes!(cluster::join))
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(settings::reset_telemetry_id))
        .routes(routes!(telemetry::preview_telemetry))
        .routes(routes!(
            subscription::get_subscription,
            subscription::put_subscription,
            subscription::delete_subscription
        ))
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::patch_pool, r::delete_pool))
        .routes(routes!(r::list_networks, r::create_network))
        .routes(routes!(r::get_network, r::delete_network))
        .routes(routes!(
            r::list_security_groups,
            security_groups::create_security_group
        ))
        .routes(routes!(
            r::get_security_group,
            security_groups::patch_security_group,
            security_groups::delete_security_group
        ))
        .routes(routes!(r::list_ha_groups, ha_groups::create_ha_group))
        .routes(routes!(
            r::get_ha_group,
            ha_groups::update_ha_group,
            ha_groups::delete_ha_group
        ))
        .routes(routes!(
            r::list_placement_rules,
            placement_rules::create_placement_rule
        ))
        .routes(routes!(
            r::get_placement_rule,
            placement_rules::update_placement_rule,
            placement_rules::delete_placement_rule
        ))
        .routes(routes!(r::list_tasks))
        .routes(routes!(r::get_task))
        .routes(routes!(r::list_vms, vms::create_vm))
        .routes(routes!(r::get_vm, vms::update_vm, vms::delete_vm))
        .routes(routes!(vms::vm_action))
        .routes(routes!(vms::migrate_vm))
        .routes(routes!(metrics::get_vm_metrics))
        .routes(routes!(snapshots::list_snapshots))
        .routes(routes!(
            snapshots::list_vm_snapshots,
            snapshots::create_snapshot
        ))
        .routes(routes!(snapshots::get_snapshot, snapshots::delete_snapshot))
        .routes(routes!(snapshots::revert_snapshot))
        .routes(routes!(snapshots::clone_vm))
        .routes(routes!(backups::list_backups))
        .routes(routes!(backups::get_backup, backups::delete_backup))
        .routes(routes!(backups::create_backup))
        .routes(routes!(backups::restore_backup))
        .routes(routes!(backups::replicate_backup))
        .routes(routes!(backup_receive::receive_backup))
        .routes(routes!(backup_receive::put_backup_file))
        .routes(routes!(backup_receive::complete_backup))
        .routes(routes!(r::list_backup_jobs, backup_jobs::create_backup_job))
        .routes(routes!(
            r::get_backup_job,
            backup_jobs::patch_backup_job,
            backup_jobs::delete_backup_job
        ))
        .routes(routes!(backup_jobs::run_backup_job))
        .routes(routes!(r::list_vm_imports))
        .routes(routes!(r::get_vm_import, vm_imports::delete_import))
        .routes(routes!(vm_imports::run_import))
        .routes(routes!(vm_imports::upload_ova))
        .routes(routes!(vm_imports::check_ova))
        .routes(routes!(vm_imports::browse_esxi))
        .routes(routes!(vm_imports::create_esxi))
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(volumes::check_import_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
        .routes(routes!(volumes::resize_volume))
        .routes(routes!(console::console))
        .routes(routes!(watch::watch))
        .routes(routes!(audit::list_audit))
}

/// API 문서. 라우터와 같은 정의에서 만들므로 경로가 어긋나지 않는다.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api().into_openapi()
}

/// `/api/v1` 아래의 API와 인증 없이 읽는 `/api/v1/openapi.json`
pub fn router(state: Arc<AppState>) -> Router {
    let (api, doc) = api().split_for_parts();
    let doc = Arc::new(doc);
    // 라우트가 맞은 요청만 감사 로그에 적는다(라우트 템플릿이 동작 이름이다).
    let api = api
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            audit::layer,
        ))
        .route(
            "/openapi.json",
            get(move || {
                let doc = doc.clone();
                async move { axum::Json(doc.as_ref().clone()) }
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            revision::layer,
        ));
    // 믿는 프록시의 헤더와 허용 출처(CORS)는 라우트를 찾기 전에 본다(preflight는 라우트가 없다).
    Router::new()
        .nest("/api/v1", api)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            proxy::layer,
        ))
        .with_state(state)
}
