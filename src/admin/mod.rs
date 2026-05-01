mod api_clients;
mod api_keys;
pub(crate) mod auth;
mod backfill;
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
            "/lexicons/{id}",
            get(lexicons::get_lexicon).delete(lexicons::delete_lexicon),
        )
        .route("/stats", get(stats::stats))
        .route("/backfill", post(backfill::create_backfill))
        .route("/backfill/status", get(backfill::backfill_status))
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
        .route(
            "/records/collection",
            delete(records::delete_collection_records),
        )
        .route(
            "/network-lexicons",
            post(network_lexicons::add).get(network_lexicons::list),
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
}
