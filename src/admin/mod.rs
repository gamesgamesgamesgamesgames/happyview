mod api_clients;
mod api_keys;
pub(crate) mod auth;
pub mod backfill;
mod dead_letters;
mod domains;
mod events;
mod feature_flags;
mod labelers;
mod lexicons;
mod network_lexicons;
pub(crate) mod permissions;
mod plugins;
mod proxy_config;
mod records;
mod script_variables;
mod scripts;
mod service_entries;
mod service_identity;
pub mod settings;
mod stats;
pub(crate) mod types;
mod users;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::AppState;

pub fn admin_routes(_state: AppState) -> Router<AppState> {
    Router::new()
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
        .route("/stats", get(stats::stats))
        .route("/backfill", post(backfill::create_backfill))
        .route("/backfill/status", get(backfill::backfill_status))
        .route(
            "/backfill/details",
            delete(backfill::flush_all_backfill_details),
        )
        .route("/backfill/{id}/cancel", post(backfill::cancel_backfill))
        .route("/backfill/{id}/pause", post(backfill::pause_backfill))
        .route("/backfill/{id}/resume", post(backfill::resume_backfill))
        .route("/backfill/{id}/events", get(backfill::backfill_events))
        .route("/backfill/{id}/repos", get(backfill::backfill_repos))
        .route(
            "/backfill/{id}/pds-summary",
            get(backfill::backfill_pds_summary),
        )
        .route(
            "/backfill/{id}/details",
            delete(backfill::flush_backfill_details),
        )
        .route("/events", get(events::list_events))
        .route("/users", post(users::create_user).get(users::list_users))
        .route("/users/transfer-super", post(users::transfer_super))
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
        .route("/scripts", get(scripts::list).post(scripts::upsert))
        .route(
            "/scripts/{id}",
            get(scripts::get)
                .patch(scripts::patch)
                .delete(scripts::delete),
        )
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
            "/api-clients",
            post(api_clients::create_api_client).get(api_clients::list_api_clients),
        )
        .route(
            "/api-clients/{id}",
            get(api_clients::get_api_client)
                .put(api_clients::update_api_client)
                .delete(api_clients::delete_api_client),
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
}
