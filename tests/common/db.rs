use happyview::db::{self, DatabaseBackend};
use sqlx::AnyPool;

pub async fn test_pool() -> AnyPool {
    let url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set for e2e tests");

    let backend = DatabaseBackend::from_url(&url);
    db::connect(&url, backend).await
}

pub fn test_backend() -> DatabaseBackend {
    let url =
        std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL must be set for e2e tests");
    DatabaseBackend::from_url(&url)
}

/// Insert a minimal `happyview_oauth_sessions` row directly, bypassing
/// `DbSessionStore::set()`. Rotation tests need to plant a session pinned to
/// a specific (or absent) `signing_kid` without driving a real OAuth flow —
/// `session_data` is never read by anything these tests exercise, so an
/// empty JSON object is enough.
pub async fn insert_oauth_session(
    pool: &AnyPool,
    backend: DatabaseBackend,
    did: &str,
    signing_kid: Option<&str>,
) {
    let sql = db::adapt_sql(
        "INSERT INTO happyview_oauth_sessions (did, session_data, signing_kid) VALUES (?, ?, ?)",
        backend,
    );
    db::query(&sql)
        .bind(did)
        .bind("{}")
        .bind(signing_kid)
        .execute(pool)
        .await
        .expect("failed to insert oauth session fixture");
}

/// Acquire a cross-process advisory lock via a dedicated Postgres connection pool.
/// The lock is held on a connection within the returned pool. When the pool is dropped,
/// the connection closes and the advisory lock is released.
/// For SQLite, returns None (no cross-process locking needed).
pub async fn acquire_test_lock() -> Option<AnyPool> {
    let url = std::env::var("TEST_DATABASE_URL").ok()?;
    let backend = DatabaseBackend::from_url(&url);

    if !matches!(backend, DatabaseBackend::Postgres) {
        return None;
    }

    sqlx::any::install_default_drivers();

    let lock_pool = sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("failed to create advisory lock pool");

    happyview::db::query("SELECT pg_advisory_lock(42)")
        .execute(&lock_pool)
        .await
        .expect("failed to acquire advisory lock");

    Some(lock_pool)
}

pub async fn truncate_all(pool: &AnyPool) {
    let backend = test_backend();
    match backend {
        DatabaseBackend::Postgres => {
            happyview::db::query(
                "TRUNCATE happyview_records, happyview_lexicons, happyview_backfill_jobs, happyview_users, happyview_user_permissions, happyview_api_keys, happyview_event_logs, happyview_script_variables, happyview_scripts, happyview_dead_letter_scripts, happyview_dead_letter_hooks, happyview_record_refs, happyview_labeler_subscriptions, happyview_labels, happyview_instance_settings, happyview_domains, happyview_dpop_sessions, happyview_dpop_keys, happyview_api_clients, happyview_api_client_probes, happyview_delegated_accounts, happyview_account_delegates, happyview_service_identity, happyview_service_entries, happyview_service_entry_xrpcs, happyview_jobs, happyview_spaces, happyview_space_members, happyview_space_records, happyview_space_repo_state, happyview_space_record_oplog, happyview_space_notify_registrations, happyview_space_invites, happyview_linked_repo_sessions, happyview_linked_repo_auth_state, happyview_linked_repos, happyview_oauth_sessions, happyview_auth_login_redirects, happyview_oauth_client_keys RESTART IDENTITY CASCADE",
            )
            .execute(pool)
            .await
            .expect("failed to truncate tables");
        }
        DatabaseBackend::Sqlite => {
            let tables = [
                "happyview_linked_repo_sessions",
                "happyview_linked_repo_auth_state",
                "happyview_linked_repos",
                "happyview_auth_login_redirects",
                // Spaces tables (children before parents — no cascade on SQLite).
                "happyview_space_credentials",
                "happyview_space_dids",
                "happyview_space_invites",
                "happyview_space_notify_registrations",
                "happyview_space_record_oplog",
                "happyview_space_repo_state",
                "happyview_space_records",
                "happyview_space_members",
                "happyview_spaces",
                "happyview_service_entry_xrpcs",
                "happyview_service_entries",
                "happyview_service_identity",
                "happyview_account_delegates",
                "happyview_delegated_accounts",
                "happyview_dpop_sessions",
                "happyview_dpop_keys",
                "happyview_api_client_probes",
                "happyview_api_clients",
                "happyview_records",
                "happyview_lexicons",
                "happyview_backfill_jobs",
                "happyview_users",
                "happyview_user_permissions",
                "happyview_api_keys",
                "happyview_event_logs",
                "happyview_script_variables",
                "happyview_scripts",
                "happyview_dead_letter_scripts",
                "happyview_dead_letter_hooks",
                "happyview_record_refs",
                "happyview_labeler_subscriptions",
                "happyview_labels",
                "happyview_instance_settings",
                "happyview_domains",
                "happyview_jobs",
                "happyview_oauth_sessions",
                "happyview_oauth_client_keys",
            ];
            for table in tables {
                happyview::db::query(&format!("DELETE FROM {table}"))
                    .execute(pool)
                    .await
                    .unwrap_or_else(|e| panic!("failed to delete from {table}: {e}"));
            }
        }
    }
}
