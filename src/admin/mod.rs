pub(crate) mod api_clients;
mod api_keys;
pub(crate) mod auth;
mod database;
mod dead_letters;
pub mod default_plugins;
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
mod project;
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
        .route(
            "/project",
            put(project::put_project).get(project::get_project).layer(
                axum::extract::DefaultBodyLimit::max(
                    crate::project::deploy::bundle::MAX_COMPRESSED_BYTES,
                ),
            ),
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
        .route("/scripts/languages", get(scripts::languages))
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

/// Admin `GET` routes read; they do not write.
///
/// The CSRF guard (`crate::csrf`) exempts safe methods, because a browser
/// cannot be made to send one with a side effect — so a `GET` that *does*
/// have a side effect is reachable from any site that can make the operator's
/// browser follow a link, with the session cookie attached. Nothing in the
/// type system says a handler behind `get(...)` only reads, and a name is no
/// guard either: `list`, `status` and `preview` are all what a mutating
/// handler would be called if someone added one by mistake.
///
/// So this reads the router, finds every handler behind `get(...)`, and reads
/// the handler itself.
///
/// **What it does not cover**: the record source's routes, which `admin_routes`
/// merges in from `state.source.admin_routes()`. They are another crate's
/// router and this cannot see into it.
#[cfg(test)]
mod get_routes_are_reads {
    use std::path::{Path, PathBuf};

    /// A permission that only reads. Everything else counts as a write, so a
    /// permission added later is a write until someone says otherwise —
    /// which is the safe direction for a guard like this.
    const READ_SUFFIXES: [&str; 2] = [":read", ":view"];

    /// `GET` handlers that require a manage-level permission and only read,
    /// because that permission has no read-level counterpart: there is no
    /// `settings:read`, no `spaces:read-records` and no `project:read`.
    ///
    /// Every entry here was checked against
    /// [`no_admin_get_handler_writes`], which finds no write statement in any
    /// of them. Adding to this list is a deliberate act: it says "this GET
    /// needs a manage permission to *read* something", not "this GET may
    /// write".
    const READS_GATED_ON_MANAGE: [&str; 16] = [
        "api_clients::list_instance_keys",
        "database::status",
        "feature_flags::list",
        "project::get_project",
        "proxy_config::get",
        "service_entries::lexicon_services",
        "service_entries::list",
        "service_entries::list_xrpcs",
        "service_identity::get",
        "settings::db_info",
        "settings::list",
        "spaces::get_space_blob",
        "spaces::list_space_records",
        "telemetry::get",
        "telemetry::preview",
        "verification_methods::list",
    ];

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Every `module::function` the admin router mounts behind `get(...)`.
    fn get_handlers() -> Vec<String> {
        let source = read(&root().join("src/admin/mod.rs"));
        // Everything from `admin_routes` to the end of the file: the router,
        // and nothing of this test module, which also contains `get(`.
        let router = source
            .split_once("pub fn admin_routes")
            .expect("admin_routes should be here")
            .1;
        let router = router
            .split_once("mod get_routes_are_reads")
            .map_or(router, |(before, _)| before);

        let mut found = Vec::new();
        for chunk in router.split(".route(").skip(1) {
            let Some(after_get) = chunk.split_once("get(") else {
                continue;
            };
            // Only the first `get(` of a route: a `.post(...)` after it is a
            // different method and not this test's business.
            let handler: String = after_get
                .1
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            if handler.contains("::") {
                found.push(handler);
            }
        }
        assert!(
            found.len() > 40,
            "the router parse found only {} GET handlers, so it has stopped working",
            found.len()
        );
        found
    }

    /// A handler's body: from its `fn` line to the next item at column 0.
    fn body_of(handler: &str) -> Option<String> {
        let (module, name) = handler.split_once("::")?;
        let path = root().join(format!("src/admin/{module}.rs"));
        if !path.exists() {
            return None;
        }
        let source = read(&path);
        let marker = format!("async fn {name}");
        let start = source.find(&marker)?;
        let after = &source[start + marker.len()..];
        let end = after
            .find("\n}\n")
            .map_or(after.len(), |i| i + "\n}\n".len());
        Some(after[..end].to_string())
    }

    /// The string literals in `body`, where a SQL statement would be.
    fn literals(body: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes: Vec<char> = body.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == '"' {
                let mut j = i + 1;
                let mut lit = String::new();
                while j < bytes.len() && bytes[j] != '"' {
                    if bytes[j] == '\\' {
                        j += 1;
                    }
                    if j < bytes.len() {
                        lit.push(bytes[j]);
                    }
                    j += 1;
                }
                out.push(lit);
                i = j + 1;
            } else {
                i += 1;
            }
        }
        out
    }

    /// No admin `GET` handler carries a write statement.
    ///
    /// Matched inside string literals only, because `UPDATE` in a sentence and
    /// a `serde_json::Map::remove` are not writes and flagging them would
    /// teach everyone to ignore this test.
    #[test]
    fn no_admin_get_handler_writes() {
        let verbs = [
            concat!("INSERT", " INTO"),
            concat!("DELETE", " FROM"),
            concat!("UPDATE", " happyview"),
            concat!("ALTER", " TABLE"),
            concat!("DROP", " TABLE"),
            concat!("TRUNCATE", " TABLE"),
        ];
        let mut offenders = Vec::new();
        for handler in get_handlers() {
            let Some(body) = body_of(&handler) else {
                continue;
            };
            for literal in literals(&body) {
                let upper = literal.to_ascii_uppercase();
                for verb in verbs {
                    if upper.contains(verb) {
                        offenders.push(format!("{handler}: {verb} in {literal:?}"));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "an admin GET route writes, and the CSRF guard exempts safe methods:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// An admin `GET` handler that requires a write permission is either a
    /// mistake or a read that has no read-level permission to ask for. The
    /// second kind is listed; the first kind fails here.
    #[test]
    fn a_get_handler_requiring_a_write_permission_is_listed() {
        let permissions = read(&root().join("src/admin/permissions.rs"));
        let is_write = |variant: &str| -> bool {
            let marker = format!("Self::{variant} => \"");
            match permissions.split_once(&marker) {
                Some((_, rest)) => {
                    let name = rest.split('"').next().unwrap_or_default();
                    !READ_SUFFIXES.iter().any(|s| name.ends_with(s))
                }
                // Unknown to the enum's own mapping: treat as a write.
                None => true,
            }
        };

        let mut unlisted = Vec::new();
        for handler in get_handlers() {
            let Some(body) = body_of(&handler) else {
                continue;
            };
            let mut writes = Vec::new();
            let mut rest = body.as_str();
            while let Some((_, tail)) = rest.split_once("Permission::") {
                let variant: String = tail
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if !variant.is_empty() && is_write(&variant) {
                    writes.push(variant);
                }
                rest = tail;
            }
            writes.sort();
            writes.dedup();
            if !writes.is_empty() && !READS_GATED_ON_MANAGE.contains(&handler.as_str()) {
                unlisted.push(format!("{handler} requires {}", writes.join(", ")));
            }
        }
        assert!(
            unlisted.is_empty(),
            "an admin GET route requires a write permission. If it only reads, add it to \
             READS_GATED_ON_MANAGE with a word about why; if it writes, it must not be a GET, \
             because the CSRF guard exempts safe methods:\n  {}",
            unlisted.join("\n  ")
        );
    }

    /// The list earns its place: every entry is a handler the router still
    /// mounts behind `get(...)`.
    #[test]
    fn the_listed_reads_are_still_get_routes() {
        let mounted = get_handlers();
        let stale: Vec<&str> = READS_GATED_ON_MANAGE
            .iter()
            .copied()
            .filter(|listed| !mounted.iter().any(|m| m == listed))
            .collect();
        assert!(
            stale.is_empty(),
            "READS_GATED_ON_MANAGE names handlers the router no longer mounts as GET: {stale:?}"
        );
    }
}
