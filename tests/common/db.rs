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
                "TRUNCATE happyview_records, happyview_lexicons, happyview_backfill_jobs, happyview_users, happyview_user_permissions, happyview_api_keys, happyview_event_logs, happyview_script_variables, happyview_scripts, happyview_dead_letter_scripts, happyview_dead_letter_hooks, happyview_record_refs, happyview_labeler_subscriptions, happyview_labels, happyview_instance_settings, happyview_domains, happyview_dpop_sessions, happyview_dpop_keys, happyview_api_clients, happyview_api_client_probes, happyview_delegated_accounts, happyview_account_delegates, happyview_service_identity, happyview_service_entries, happyview_service_entry_xrpcs, happyview_jobs, happyview_spaces, happyview_space_members, happyview_space_records, happyview_space_repo_state, happyview_space_record_oplog, happyview_space_notify_registrations, happyview_space_invites, happyview_linked_repo_sessions, happyview_linked_repo_auth_state, happyview_linked_repos, happyview_oauth_sessions, happyview_auth_login_redirects, happyview_oauth_client_keys, happyview_verification_methods RESTART IDENTITY CASCADE",
            )
            .execute(pool)
            .await
            .expect("failed to truncate tables");
        }
        DatabaseBackend::Sqlite => {
            let tables = [
                // Reset between apps: a key provisioned under one encryption
                // key cannot be decrypted with the next test's, which surfaces
                // as an opaque 500 on the first space write.
                "happyview_verification_methods",
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

/// The encryption key every test uses.
///
/// Must match `happyview::test_support::TEST_ENCRYPTION_KEY`, which is
/// `#[cfg(test)]` and so not visible here. Integration and lib tests share a
/// database; see that constant for why the values must agree.
pub const TEST_ENCRYPTION_KEY: [u8; 32] = [0x42u8; 32];

/// Provision the `#atproto_space` signing key, replacing one this state cannot
/// decrypt.
///
/// A running instance provisions the key at startup and in `create_space`, and
/// the space write path only reads it. Tests that seed spaces straight into the
/// DB bypass both, so without this every space write returns 500 on a missing
/// key.
pub async fn provision_space_signing_key(
    pool: &AnyPool,
    backend: DatabaseBackend,
    encryption_key: &[u8; 32],
) {
    let usable = happyview::verification_methods::get_private_key_bytes(
        pool,
        backend,
        "#atproto_space",
        encryption_key,
    )
    .await
    .ok()
    .flatten()
    .is_some();

    if !usable {
        let sql = happyview::db::adapt_sql(
            "DELETE FROM happyview_verification_methods WHERE fragment_id = ?",
            backend,
        );
        let _ = happyview::db::query(&sql)
            .bind("#atproto_space")
            .execute(pool)
            .await;
    }

    happyview::verification_methods::ensure_atproto_space_method(pool, backend, encryption_key)
        .await
        .expect("provision #atproto_space key");
}
