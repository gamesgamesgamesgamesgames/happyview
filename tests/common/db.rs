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

pub async fn truncate_all(pool: &AnyPool) {
    let backend = test_backend();
    match backend {
        DatabaseBackend::Postgres => {
            sqlx::query(
                "TRUNCATE records, lexicons, backfill_jobs, users, user_permissions, api_keys, event_logs, script_variables, scripts, dead_letter_scripts, dead_letter_hooks, record_refs, labeler_subscriptions, labels, instance_settings, domains, dpop_sessions, dpop_keys, api_clients, delegated_accounts, account_delegates RESTART IDENTITY CASCADE",
            )
            .execute(pool)
            .await
            .expect("failed to truncate tables");
        }
        DatabaseBackend::Sqlite => {
            let tables = [
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
