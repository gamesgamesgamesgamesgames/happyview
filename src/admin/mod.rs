pub(crate) mod api_clients;
mod api_keys;
pub(crate) mod auth;
mod database;
mod dead_letters;
mod domains;
mod editor;
mod events;
mod feature_flags;
mod identity;
mod jobs;
mod labelers;
mod lexicon_publishing;
mod lexicons;
mod linked_repos;
pub(crate) mod network_lexicons;
pub(crate) mod permissions;
mod platform;
mod plugins;
mod proxy_config;
mod records;
mod script_variables;
mod scripts;
mod service_entries;
mod service_identity;
pub mod settings;
mod spaces;
pub mod spaces_migration;
mod stats;
mod storage;
mod telemetry;
pub(crate) mod types;
mod users;
mod verification_methods;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::AppState;

pub fn admin_routes(state: AppState) -> Router<AppState> {
    // The record source's routes, each behind the admin auth layer. Its
    // router carries its own state, so it joins this one as a stateless one.
    let source_routes = state
        .source
        .admin_routes()
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin_caller,
        ))
        .with_state(());
    Router::new()
        .merge(source_routes)
        .route(
            "/lexicons",
            post(lexicons::upload_lexicon).get(lexicons::list_lexicons),
        )
        .route(
            "/lexicons/{id}/services",
            get(service_entries::lexicon_services),
        )
        .route(
            "/lexicons/{id}",
            get(lexicons::get_lexicon).delete(lexicons::delete_lexicon),
        )
        .route("/lexicon-publishing", get(lexicon_publishing::overview))
        .route("/lexicon-publishing/dns", get(lexicon_publishing::dns))
        .route(
            "/lexicon-publishing/dns/{nsid}",
            get(lexicon_publishing::dns_for_lexicon),
        )
        .route(
            "/lexicon-publishing/rules",
            post(lexicon_publishing::create_rule),
        )
        .route(
            "/lexicon-publishing/rules/{id}",
            patch(lexicon_publishing::update_rule).delete(lexicon_publishing::delete_rule),
        )
        .route(
            "/lexicon-publishing/publish",
            post(lexicon_publishing::publish),
        )
        .route(
            "/lexicon-publishing/unpublish",
            post(lexicon_publishing::unpublish),
        )
        .route("/stats", get(stats::stats))
        .route("/stats/counts", get(stats::counts))
        .route(
            "/spaces/migration-status",
            get(spaces_migration::migration_status),
        )
        .route("/spaces", get(spaces::list_spaces))
        .route("/spaces/{id}", get(spaces::get_space))
        .route("/spaces/{id}/records", get(spaces::list_space_records))
        .route("/spaces/{id}/blob", get(spaces::get_space_blob))
        .route("/jobs", get(jobs::list_jobs))
        .route("/jobs/{id}", get(jobs::get_job))
        .route("/jobs/{id}/cancel", post(jobs::cancel_job))
        .route("/jobs/{id}/pause", post(jobs::pause_job))
        .route("/jobs/{id}/resume", post(jobs::resume_job))
        .route("/jobs/{id}/logs", get(jobs::list_job_logs))
        .route(
            "/linked-repos",
            get(linked_repos::list_linked_repos).post(linked_repos::create_linked_repo),
        )
        .route(
            "/linked-repos/{id}",
            delete(linked_repos::delete_linked_repo),
        )
        .route(
            "/linked-repos/{id}/authorize",
            post(linked_repos::authorize_linked_repo),
        )
        .route(
            "/linked-repos/{id}/invite",
            post(linked_repos::invite_linked_repo),
        )
        .route(
            "/linked-repos/{id}/invites",
            get(linked_repos::list_linked_repo_invites),
        )
        .route(
            "/linked-repos/{id}/invites/{invite_id}",
            delete(linked_repos::revoke_linked_repo_invite),
        )
        .route("/events", get(events::list_events))
        .route("/events/count", get(events::count_events))
        .route("/events/purge", post(events::purge_events))
        .route("/identity/resolve", get(identity::resolve_identity))
        .route("/users", post(users::create_user).get(users::list_users))
        .route("/users/transfer-super", post(users::transfer_super))
        .route("/platform/super-user", put(platform::set_super_user))
        .route(
            "/users/{id}",
            get(users::get_user).delete(users::delete_user),
        )
        .route("/users/{id}/permissions", patch(users::update_permissions))
        .route(
            "/api-keys",
            post(api_keys::create_api_key).get(api_keys::list_api_keys),
        )
        .route("/api-keys/{id}", delete(api_keys::revoke_api_key))
        .route(
            "/records",
            get(records::list_records).delete(records::delete_record),
        )
        .route("/records/collections", get(records::list_collections))
        .route(
            "/records/collection",
            delete(records::delete_collection_records),
        )
        .route("/database/status", get(database::status))
        .route(
            "/database/vacuum/schedule",
            post(database::schedule_vacuum).delete(database::cancel_vacuum),
        )
        .route("/storage/retrain", post(storage::retrain))
        .route(
            "/network-lexicons",
            post(network_lexicons::add).get(network_lexicons::list),
        )
        .route(
            "/network-lexicons/resolve/{nsid}",
            get(network_lexicons::resolve),
        )
        .route("/network-lexicons/{nsid}", delete(network_lexicons::remove))
        .route(
            "/script-variables",
            post(script_variables::upsert).get(script_variables::list),
        )
        .route("/script-variables/{key}", delete(script_variables::delete))
        .route("/editor/declarations", get(editor::declarations))
        .route("/scripts", get(scripts::list).post(scripts::upsert))
        .route(
            "/scripts/{id}",
            get(scripts::get)
                .patch(scripts::patch)
                .delete(scripts::delete),
        )
        .route("/scripts/{id}/codemod", post(scripts::codemod_apply))
        .route("/labelers", post(labelers::add).get(labelers::list))
        .route(
            "/labelers/{did}",
            patch(labelers::update).delete(labelers::delete),
        )
        .route("/feature-flags", get(feature_flags::list))
        .route("/settings", get(settings::list))
        .route("/settings/db-info", get(settings::db_info))
        .route(
            "/settings/logo",
            put(settings::upload_logo).delete(settings::delete_logo),
        )
        .route(
            "/settings/xrpc-proxy",
            get(proxy_config::get).put(proxy_config::put),
        )
        .route(
            "/settings/telemetry",
            get(telemetry::get).put(telemetry::update),
        )
        .route("/settings/telemetry/dismiss", post(telemetry::dismiss))
        .route("/settings/telemetry/preview", get(telemetry::preview))
        .route("/settings/telemetry/send", post(telemetry::send))
        .route(
            "/settings/{key}",
            put(settings::upsert).delete(settings::delete),
        )
        .route("/plugins", post(plugins::add).get(plugins::list))
        .route("/plugins/preview", post(plugins::preview))
        .route("/plugins/official", get(plugins::list_official))
        .route("/plugins/{id}", delete(plugins::remove))
        .route("/plugins/{id}/reload", post(plugins::reload))
        .route("/plugins/{id}/check-update", post(plugins::check_update))
        .route(
            "/plugins/{id}/secrets",
            get(plugins::get_secrets).put(plugins::update_secrets),
        )
        .route(
            "/plugins/{id}/allowed-hosts",
            get(plugins::get_allowed_hosts).put(plugins::update_allowed_hosts),
        )
        .route(
            "/api-clients",
            post(api_clients::create_api_client).get(api_clients::list_api_clients),
        )
        .route(
            "/api-clients/{id}",
            get(api_clients::get_api_client)
                .put(api_clients::update_api_client)
                .delete(api_clients::delete_api_client),
        )
        .route(
            "/api-clients/{id}/auth-key",
            post(api_clients::provision_auth_key).get(api_clients::get_auth_key),
        )
        .route(
            "/api-clients/{id}/auth-key/recheck",
            post(api_clients::recheck_auth_key),
        )
        .route(
            "/api-clients/{id}/auth-key/rotate",
            post(api_clients::rotate_auth_key),
        )
        .route(
            "/api-clients/{id}/auth-keys",
            get(api_clients::list_auth_keys).delete(api_clients::revoke_all_auth_keys),
        )
        .route(
            "/api-clients/{id}/auth-key/{kid}",
            delete(api_clients::revoke_auth_key),
        )
        .route(
            "/oauth/instance-key/rotate",
            post(api_clients::rotate_instance_key),
        )
        .route("/oauth/instance-key", get(api_clients::list_instance_keys))
        .route(
            "/oauth/instance-key/{kid}",
            delete(api_clients::revoke_instance_key),
        )
        .route("/domains", post(domains::create).get(domains::list))
        .route("/domains/{id}", delete(domains::delete))
        .route("/domains/{id}/primary", post(domains::set_primary))
        .route("/dead-letters", get(dead_letters::list))
        .route("/dead-letters/count", get(dead_letters::count))
        .route(
            "/dead-letters/bulk/dismiss",
            post(dead_letters::bulk_dismiss),
        )
        .route("/dead-letters/bulk/retry", post(dead_letters::bulk_retry))
        .route(
            "/dead-letters/bulk/reindex",
            post(dead_letters::bulk_reindex),
        )
        .route("/dead-letters/{id}", get(dead_letters::detail))
        .route("/dead-letters/{id}/dismiss", post(dead_letters::dismiss))
        .route("/dead-letters/{id}/retry", post(dead_letters::retry))
        .route("/dead-letters/{id}/reindex", post(dead_letters::reindex))
        .route("/permissions", get(users::list_permissions))
        .route(
            "/service-identity",
            get(service_identity::get).put(service_identity::update),
        )
        .route(
            "/service-entries",
            get(service_entries::list).post(service_entries::create),
        )
        .route("/service-entries/sync-plc", post(service_entries::sync_plc))
        .route(
            "/service-entries/sync-plc/request",
            post(service_entries::sync_plc_request),
        )
        .route(
            "/service-entries/sync-plc/submit",
            post(service_entries::sync_plc_submit),
        )
        .route(
            "/service-entries/{id}",
            put(service_entries::update).delete(service_entries::delete),
        )
        .route(
            "/service-entries/{id}/xrpcs",
            get(service_entries::list_xrpcs)
                .post(service_entries::add_xrpcs)
                .delete(service_entries::remove_xrpcs),
        )
        .route(
            "/verification-methods",
            get(verification_methods::list).post(verification_methods::create),
        )
        .route(
            "/verification-methods/{fragment_id}",
            delete(verification_methods::delete),
        )
}
