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

    sqlx::query("SELECT pg_advisory_lock(42)")
        .execute(&lock_pool)
        .await
        .expect("failed to acquire advisory lock");

    Some(lock_pool)
}

pub async fn truncate_all(pool: &AnyPool) {
    let backend = test_backend();
    match backend {
        DatabaseBackend::Postgres => {
            sqlx::query(
                "TRUNCATE records, lexicons, backfill_jobs, users, user_permissions, api_keys, event_logs, script_variables, scripts, dead_letter_scripts, dead_letter_hooks, record_refs, labeler_subscriptions, labels, instance_settings, domains, dpop_sessions, dpop_keys, api_clients, delegated_accounts, account_delegates, service_identity, service_entries, service_entry_xrpcs RESTART IDENTITY CASCADE",
            )
            .execute(pool)
            .await
            .expect("failed to truncate tables");
        }
        DatabaseBackend::Sqlite => {
            let tables = [
                "service_entry_xrpcs",
                "service_entries",
                "service_identity",
                "account_delegates",
                "delegated_accounts",
                "dpop_sessions",
                "dpop_keys",
                "api_clients",
                "records",
                "lexicons",
                "backfill_jobs",
                "users",
                "user_permissions",
                "api_keys",
                "event_logs",
                "script_variables",
                "scripts",
                "dead_letter_scripts",
                "dead_letter_hooks",
                "record_refs",
                "labeler_subscriptions",
                "labels",
                "instance_settings",
                "domains",
            ];
            for table in tables {
                sqlx::query(&format!("DELETE FROM {table}"))
                    .execute(pool)
                    .await
                    .unwrap_or_else(|e| panic!("failed to delete from {table}: {e}"));
            }
        }
    }
}
