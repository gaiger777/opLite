//! Orbivisor REST API(`/api/v1`), Task 실행기, 인증.
//!
//! API는 원하는 상태(spec)를 Raft에 기록하고 Task를 만들기만 한다. VM을 실제로 움직이는 것은
//! 각 노드의 reconciler이고, Task의 진행은 대상 노드의 [`TaskRunner`]가 기록한다.

pub mod acme;
pub mod alerts;
mod api_cert;
pub mod audit;
pub mod auth;
pub mod authz;
mod authz_routes;
mod backup_jobs;
mod backup_receive;
mod backup_stores;
mod backups;
pub mod balance;
mod balancer;
mod ceph;
mod ceph_agent;
pub mod cluster;
mod console;
mod cpu;
mod cpu_baseline;
mod dashboard;
mod declarative;
pub mod dr;
mod drain;
mod drift;
pub mod error;
mod fan_out;
pub mod file_restore;
mod firmware;
mod folders;
mod guest;
mod ha;
mod ha_checks;
mod ha_groups;
mod ha_health;
pub mod health;
mod history;
mod images;
mod incoming;
mod ipam;
mod janitor;
pub mod ldap;
pub mod limit;
mod load_balancers;
mod metrics;
mod metrics_history;
mod migration_disks;
mod network_migrations;
mod node_fence;
mod numa;
pub mod oidc;
mod oidc_login;
pub mod ovn_sync;
mod permissions;
pub mod placement;
mod placement_api;
pub mod placement_board;
mod placement_rules;
pub mod power_fence;
mod prometheus;
pub mod proxy;
mod quota;
pub mod realms;
pub mod remote_client;
mod remote_proxy;
mod remotes;
mod replica_receive;
mod replication;
pub mod resources;
pub mod revision;
mod routing;
mod schedule;
pub mod scheduler;
pub mod score;
mod sdn;
mod security_groups;
mod settings;
mod snapshots;
mod state;
pub mod stores;
mod subscription;
mod tasks;
mod telemetry;
mod topology;
pub mod two_factor;
mod updates;
pub mod users;
pub mod vip;
mod vm_checks;
pub mod vm_exports;
mod vm_firmware;
pub mod vm_imports;
mod vms;
pub mod vmware;
pub mod vmware_migrations;
mod vmware_scheduler;
pub mod vmware_sources;
mod volumes;
mod watch;
pub mod webauthn;

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use alerts::{ALERTS_DIR, AlertBook, AlertEngine, AlertEngineConfig, SenderConfig};
pub use api_cert::{ApiCertAgent, ApiCertError};
pub use audit::{AuditConfig, AuditLog};
pub use auth::{Action, Authorizer, Principal, RoleAuthorizer, hash_password};
pub use backup_jobs::{BackupJobScheduler, BackupJobSchedulerConfig};
pub use balancer::{Balancer, BalancerConfig};
pub use ceph::CephPeerSource;
pub use ceph_agent::CephAgent;
pub use cpu::CpuRefresher;
pub use dr::{DrProtection, DrProtectionConfig};
pub use drain::start_drain;
pub use drift::{DriftChecker, DriftCheckerConfig};
pub use file_restore::FileRestoreSessions;
pub use firmware::UefiVarsFetcher;
pub use ha::{HaManager, HaManagerConfig, LEASE_EXPIRED, RecoveryWriter};
pub use health::RuntimeProbe;
pub use incoming::{IncomingConfig, IncomingMigrations};
pub use janitor::{ClusterJanitor, ClusterJanitorConfig};
pub use ovn_sync::{OvnSync, OvnSyncConfig, OvnSyncer, OvnTransport, SyncOutcome};
pub use placement_board::{PlacementBoard, PlacementStatusSource};
pub use power_fence::{FenceStatusSource, Fencer, PowerFence, PowerOff, power_off};
pub use remotes::{RemotePoller, RemotePollerConfig};
pub use replica_receive::{IncomingReplicas, REPLICA_SWEEP_INTERVAL, ReplicaConfig};
pub use replication::{ReplicationScheduler, ReplicationSchedulerConfig};
pub use schedule::{SnapshotScheduler, SnapshotSchedulerConfig};
pub use state::AppState;
pub use tasks::live_restore::request_cancel as request_live_restore_cancel;
pub use tasks::sdn::update_sdn as update_sdn_status;
pub use tasks::{TaskRunner, TaskRunnerConfig};
pub use telemetry::{TelemetryConfig, TelemetryReporter};
pub use updates::{AUTH_FILE, KEYRING, UpdateAgent, Updates, UpdatesConfig};
pub use vip::{VipAgent, VipAgentConfig, VipManager, VipManagerConfig};
pub use vm_exports::ExportFiles;
pub use vmware_scheduler::{VmwareScheduler, VmwareSchedulerConfig};

use crate::declarative::ManifestSchemas;
use crate::resources as r;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Orbivisor API",
        description = "Orbivisor REST API (`/api/v1`). Every authenticated response carries `Orbivisor-Revision`, \
            the revision the answering node had applied when it answered. An authenticated request with \
            `Orbivisor-Min-Revision: <revision>` is answered once the node has applied that revision (503 \
            `revision_unavailable` after 5 seconds); pass the `revision` of a resource written elsewhere to read it. \
            Writes may carry `Orbivisor-Client: <tool>/<version>` (`cli`, `terraform`, `web`) and \
            `Orbivisor-Change-Reason: <text>` (UTF-8, at most 200 characters); both are kept in the configuration \
            history next to the change."
    ),
    servers((url = "/api/v1")),
    modifiers(&Security, &ManifestSchemas),
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
        .routes(routes!(two_factor::second_factor))
        .routes(routes!(two_factor::get_two_factor))
        .routes(routes!(two_factor::enroll_totp, two_factor::disable_totp))
        .routes(routes!(two_factor::confirm_totp))
        .routes(routes!(two_factor::regenerate_recovery))
        .routes(routes!(two_factor::start_passkey))
        .routes(routes!(two_factor::finish_passkey))
        .routes(routes!(two_factor::delete_passkey))
        .routes(routes!(two_factor::reset_two_factor))
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
        .routes(routes!(ceph::list_node_disks))
        .routes(routes!(updates::refresh_updates))
        .routes(routes!(cpu::get_node_cpu))
        .routes(routes!(cpu::refresh_node_cpu))
        .routes(routes!(numa::get_node_topology))
        .routes(routes!(numa::topology_usage))
        // 운영자 대시보드(계획 75)
        .routes(routes!(dashboard::physical_topology))
        .routes(routes!(dashboard::get_capacity))
        .routes(routes!(dashboard::forget_ports))
        .routes(routes!(updates::upgrade_node))
        .routes(routes!(drain::drain_node))
        .routes(routes!(drain::unfreeze_node))
        .routes(routes!(
            node_fence::get_node_fence,
            node_fence::put_node_fence,
            node_fence::delete_node_fence
        ))
        .routes(routes!(node_fence::test_node_fence))
        .routes(routes!(metrics::get_node_metrics))
        .routes(routes!(metrics::list_node_metrics))
        .routes(routes!(metrics_history::get_node_history))
        .routes(routes!(metrics_history::get_cluster_history))
        .routes(routes!(prometheus::get_prometheus))
        .routes(routes!(r::list_alert_rules, alerts::create_alert_rule))
        .routes(routes!(alerts::create_sdn_default_rules))
        .routes(routes!(alerts::create_link_default_rules))
        .routes(routes!(alerts::create_dr_default_rules))
        .routes(routes!(
            r::get_alert_rule,
            alerts::update_alert_rule,
            alerts::delete_alert_rule
        ))
        .routes(routes!(alerts::list_alerts))
        .routes(routes!(alerts::list_alert_history))
        .routes(routes!(
            alerts::list_notification_targets,
            alerts::create_notification_target
        ))
        .routes(routes!(
            alerts::get_notification_target,
            alerts::update_notification_target,
            alerts::delete_notification_target
        ))
        .routes(routes!(alerts::test_notification_target))
        .routes(routes!(cluster::get_cluster))
        .routes(routes!(cluster::get_ca))
        .routes(routes!(cluster::get_version))
        .routes(routes!(health::get_health))
        .routes(routes!(cluster::list_api_endpoints))
        .routes(routes!(cluster::create_join_token))
        .routes(routes!(cluster::join))
        .routes(routes!(settings::get_settings, settings::patch_settings))
        .routes(routes!(settings::reset_telemetry_id))
        .routes(routes!(cpu::list_cpu_models))
        .routes(routes!(cpu::list_cpu_flags))
        .routes(routes!(cpu::get_cpu_baseline))
        .routes(routes!(telemetry::preview_telemetry))
        .routes(routes!(
            subscription::get_subscription,
            subscription::put_subscription,
            subscription::delete_subscription
        ))
        .routes(routes!(ceph::get_ceph, ceph::init_ceph))
        .routes(routes!(ceph::get_ceph_status))
        .routes(routes!(ceph::add_mon))
        .routes(routes!(ceph::add_mgr))
        .routes(routes!(ceph::add_osd))
        .routes(routes!(ceph::create_ceph_pool))
        .routes(routes!(ceph::install_client))
        .routes(routes!(r::list_pools, r::create_pool))
        .routes(routes!(r::get_pool, r::patch_pool, r::delete_pool))
        .routes(routes!(metrics_history::get_pool_history))
        .routes(routes!(r::list_networks, r::create_network))
        .routes(routes!(
            r::get_network,
            r::delete_network,
            sdn::patch_network
        ))
        .routes(routes!(sdn::network_ips))
        .routes(routes!(topology::network_topology))
        .routes(routes!(routing::detach_network_router))
        .routes(routes!(routing::list_routers, routing::create_router))
        .routes(routes!(
            routing::get_router,
            routing::patch_router,
            routing::delete_router
        ))
        .routes(routes!(
            routing::list_floating_ips,
            routing::create_floating_ip
        ))
        .routes(routes!(
            routing::get_floating_ip,
            routing::patch_floating_ip,
            routing::delete_floating_ip
        ))
        .routes(routes!(
            load_balancers::list_load_balancers,
            load_balancers::create_load_balancer
        ))
        .routes(routes!(
            load_balancers::get_load_balancer,
            load_balancers::patch_load_balancer,
            load_balancers::delete_load_balancer
        ))
        .routes(routes!(
            network_migrations::list_network_migrations,
            network_migrations::create_network_migration
        ))
        .routes(routes!(
            network_migrations::get_network_migration,
            network_migrations::delete_network_migration
        ))
        .routes(routes!(network_migrations::start_network_migration))
        .routes(routes!(network_migrations::resume_network_migration))
        .routes(routes!(network_migrations::pause_network_migration))
        .routes(routes!(network_migrations::finish_network_migration))
        .routes(routes!(network_migrations::rollback_network_migration))
        .routes(routes!(sdn::get_sdn, sdn::init_sdn))
        .routes(routes!(sdn::add_central))
        .routes(routes!(sdn::remove_central))
        .routes(routes!(sdn::install_chassis))
        .routes(routes!(sdn::set_lockdown))
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
        .routes(routes!(r::list_ha_checks, ha_checks::create_ha_check))
        .routes(routes!(
            r::get_ha_check,
            ha_checks::update_ha_check,
            ha_checks::delete_ha_check
        ))
        .routes(routes!(placement_api::get_placement))
        .routes(routes!(placement_api::list_recommendations))
        .routes(routes!(placement_api::apply_recommendation))
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
        .routes(routes!(vm_checks::check_vm))
        .routes(routes!(r::get_vm, vms::update_vm, vms::delete_vm))
        .routes(routes!(vms::vm_action))
        .routes(routes!(vms::migrate_vm))
        .routes(routes!(vms::cdrom_media))
        .routes(routes!(vms::replicate_vm))
        .routes(routes!(vms::convert_template, vms::untemplate_vm))
        .routes(routes!(vm_firmware::reset_firmware))
        .routes(routes!(cpu::get_vm_cpu))
        .routes(routes!(numa::get_vm_numa))
        .routes(routes!(guest::get_guest))
        .routes(routes!(guest::set_guest_password))
        .routes(routes!(guest::restore_caps))
        .routes(routes!(metrics::get_vm_metrics))
        .routes(routes!(metrics_history::get_vm_history))
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
        .routes(routes!(backup_receive::missing_chunks))
        .routes(routes!(backup_receive::put_chunk))
        .routes(routes!(backups::verify_backup))
        .routes(routes!(backups::convert_backup))
        .routes(routes!(backups::copy_backup))
        .routes(routes!(file_restore::open_session))
        .routes(routes!(file_restore::list_sessions))
        .routes(routes!(
            file_restore::get_session,
            file_restore::close_session_route
        ))
        .routes(routes!(file_restore::list_entries))
        .routes(routes!(file_restore::stat_entry))
        .routes(routes!(file_restore::download_link))
        .routes(routes!(file_restore::download))
        .routes(routes!(file_restore::restore_to_vm))
        .routes(routes!(vms::cancel_live_restore))
        .routes(routes!(
            backup_stores::list_stores,
            backup_stores::create_store
        ))
        .routes(routes!(
            backup_stores::get_store,
            backup_stores::patch_store,
            backup_stores::delete_store
        ))
        .routes(routes!(backup_stores::gc_store))
        .routes(routes!(backup_stores::verify_store))
        .routes(routes!(backup_stores::check_store))
        .routes(routes!(backup_stores::scan_store))
        .routes(routes!(backup_stores::take_over))
        .routes(routes!(backup_stores::export_key))
        .routes(routes!(backup_stores::escrow_key_handler))
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
        .routes(routes!(
            vmware_sources::list_sources,
            vmware_sources::create_source
        ))
        .routes(routes!(
            vmware_sources::get_source,
            vmware_sources::patch_source,
            vmware_sources::delete_source
        ))
        .routes(routes!(vmware_sources::check_source))
        .routes(routes!(vmware_sources::source_inventory))
        // ACME 인증서(계획 73 §13)
        .routes(routes!(acme::api::list_accounts, acme::api::create_account))
        .routes(routes!(
            acme::api::get_account,
            acme::api::patch_account,
            acme::api::delete_account
        ))
        .routes(routes!(acme::api::check_account))
        .routes(routes!(acme::api::list_dns, acme::api::create_dns))
        .routes(routes!(
            acme::api::get_dns,
            acme::api::patch_dns,
            acme::api::delete_dns
        ))
        .routes(routes!(
            acme::api::list_certificates,
            acme::api::create_certificate
        ))
        .routes(routes!(
            acme::api::get_certificate,
            acme::api::patch_certificate,
            acme::api::delete_certificate
        ))
        .routes(routes!(acme::api::renew_certificate))
        .routes(routes!(
            r::list_vmware_migrations,
            vmware_migrations::create_migration
        ))
        .routes(routes!(
            r::get_vmware_migration,
            vmware_migrations::patch_migration,
            vmware_migrations::delete_migration
        ))
        .routes(routes!(vmware_migrations::preflight_migration))
        .routes(routes!(vmware_migrations::start_migration))
        .routes(routes!(vmware_migrations::pause_migration))
        .routes(routes!(vmware_migrations::resume_migration))
        .routes(routes!(vmware_migrations::cancel_migration))
        .routes(routes!(vmware_migrations::list_items))
        .routes(routes!(vmware_migrations::get_item))
        .routes(routes!(vmware_migrations::retry_item))
        .routes(routes!(vmware_migrations::cancel_item))
        .routes(routes!(vmware_migrations::cutover_item))
        .routes(routes!(vmware_migrations::revert_item))
        .routes(routes!(vmware_migrations::finalize_item))
        // DR 오케스트레이션(계획 76 §3)
        .routes(routes!(r::list_dr_plans, dr::create_plan))
        .routes(routes!(r::get_dr_plan, dr::update_plan, dr::delete_plan))
        .routes(routes!(dr::plan_report))
        .routes(routes!(dr::list_runs))
        .routes(routes!(dr::get_run))
        .routes(routes!(dr::test_plan))
        .routes(routes!(dr::failover_plan))
        .routes(routes!(dr::cleanup_plan))
        .routes(routes!(dr::cancel_plan))
        .routes(routes!(dr::reprotect_plan))
        .routes(routes!(vm_exports::export_vm))
        .routes(routes!(r::list_vm_exports))
        .routes(routes!(r::get_vm_export, vm_exports::delete_export))
        .routes(routes!(vm_exports::export_download_link))
        .routes(routes!(vm_exports::download_export))
        .routes(routes!(r::list_volumes, volumes::create_volume))
        .routes(routes!(volumes::import_volume))
        .routes(routes!(volumes::check_import_volume))
        .routes(routes!(volumes::download_volume))
        .routes(routes!(r::get_volume, volumes::delete_volume))
        .routes(routes!(volumes::resize_volume))
        .routes(routes!(volumes::flatten_volume))
        .routes(routes!(images::list_catalog, images::add_catalog_image))
        .routes(routes!(images::remove_catalog_image))
        .routes(routes!(images::download_catalog_image))
        .routes(routes!(console::console))
        .routes(routes!(watch::watch))
        .routes(routes!(audit::list_audit))
        .routes(routes!(folders::list_folders, folders::create_folder))
        .routes(routes!(
            folders::get_folder,
            folders::patch_folder,
            folders::delete_folder
        ))
        .routes(routes!(folders::move_members))
        .routes(routes!(quota::folder_usage))
        .routes(routes!(
            permissions::list_access_roles,
            permissions::create_access_role
        ))
        .routes(routes!(
            permissions::get_access_role,
            permissions::update_access_role,
            permissions::delete_access_role
        ))
        .routes(routes!(permissions::list_groups, permissions::create_group))
        .routes(routes!(
            permissions::get_group,
            permissions::update_group,
            permissions::delete_group
        ))
        .routes(routes!(permissions::list_grants, permissions::create_grant))
        .routes(routes!(permissions::get_grant, permissions::delete_grant))
        .routes(routes!(permissions::my_permissions))
        .routes(routes!(permissions::effective_permissions))
        .routes(routes!(permissions::who_permissions))
        .routes(routes!(declarative::apply))
        .routes(routes!(declarative::list_manifests))
        .routes(routes!(
            declarative::get_manifest,
            declarative::delete_manifest
        ))
        .routes(routes!(declarative::list_apply_sets))
        .routes(routes!(
            declarative::get_apply_set,
            declarative::delete_apply_set
        ))
        .routes(routes!(drift::adopt))
        .routes(routes!(history::timeline))
        .routes(routes!(history::restore_point))
        .routes(routes!(history::resource_history))
        .routes(routes!(history::history_row))
        .routes(routes!(history::restore_one))
}

/// API 문서. 라우터와 같은 정의에서 만들므로 경로가 어긋나지 않는다.
pub fn openapi() -> utoipa::openapi::OpenApi {
    api_parts().1
}

/// [`api_parts`]가 쓰는 스레드의 스택(16 MiB)
const API_STACK: usize = 16 << 20;

/// 라우트 표와 API 문서. 경로·스키마가 많아 만들 때 디버그 빌드에서 스택을 많이 쓰므로(계획 66 스테이지 1에서 2 MiB 시험
/// 스레드가 넘쳤다) 넉넉한 스택의 스레드에서 만든다.
fn api_parts() -> (Router<Arc<AppState>>, utoipa::openapi::OpenApi) {
    std::thread::Builder::new()
        .name("orbivisor-api-routes".into())
        .stack_size(API_STACK)
        .spawn(|| api().split_for_parts())
        .expect("spawn the API route builder thread")
        .join()
        .expect("building the API routes panicked")
}

/// `/api/v1` 아래의 API와 인증 없이 읽는 `/api/v1/openapi.json`
pub fn router(state: Arc<AppState>) -> Router {
    let (api, doc) = api_parts();
    let doc = Arc::new(doc);
    // 라우트가 맞은 요청만 경로표대로 권한을 보고(계획 62), 감사 로그에 적는다(라우트 템플릿이 동작 이름이다).
    // 감사가 바깥이라 권한 거절(403·404)도 적는다.
    let api = api
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            authz_routes::layer,
        ))
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
        ))
        // 두 이름의 세션 쿠키 값이 다르면 고른 쪽으로 새 이름을 맞춘다(계획 74 리뷰 2, 다음 판에서 뺌).
        .layer(axum::middleware::from_fn(auth::resync_session_cookie));
    // 믿는 프록시의 헤더와 허용 출처(CORS)는 라우트를 찾기 전에 본다(preflight는 라우트가 없다).
    Router::new()
        .nest("/api/v1", api)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            proxy::layer,
        ))
        .with_state(state)
}
