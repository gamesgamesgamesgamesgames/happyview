//! How long stored metrics are kept.
//!
//! Metrics follow the event log's retention by design: one setting,
//! `event_log_retention_days`, says how long this instance keeps what it
//! records about itself, and any cap put on the event log covers metrics
//! too. This resolves it the way the event log's cleanup does, and the two
//! must keep resolving it the same way.

use sqlx::AnyPool;

use crate::db::DatabaseBackend;

pub const KEY: &str = "event_log_retention_days";

pub const DEFAULT_DAYS: u32 = 30;

/// Days stored metrics are kept. `0` keeps them indefinitely, as it keeps
/// events; a value that is not a non-negative integer is reported and the
/// default stands.
pub async fn retention_days(db: &AnyPool, backend: DatabaseBackend) -> u32 {
    match crate::admin::settings::get_setting(db, KEY, backend).await {
        Some(v) => v.parse::<u32>().unwrap_or_else(|e| {
            tracing::warn!(
                value = %v,
                error = %e,
                "event_log_retention_days is not a valid non-negative integer, defaulting to 30"
            );
            DEFAULT_DAYS
        }),
        None => DEFAULT_DAYS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retention_resolves_as_the_event_logs_does() {
        let pool = crate::test_support::migrated_memory_pool().await;
        let backend = DatabaseBackend::Sqlite;
        // A stored value outranks the environment, which other tests in the
        // process may set, so only stored values are checked.
        for (raw, days) in [("7", 7), ("0", 0), ("banana", DEFAULT_DAYS)] {
            crate::admin::settings::put_setting(&pool, KEY, raw, backend)
                .await
                .unwrap();
            assert_eq!(retention_days(&pool, backend).await, days, "{raw}");
        }
    }
}
