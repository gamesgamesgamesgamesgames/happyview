//! Counts HappyView's telemetry and a provider's `metrics` are built from.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};

/// Days back a row still makes its feature active.
pub const ACTIVE_WINDOW_DAYS: i64 = 7;

/// `table`'s row count, or `None` when it cannot be counted.
pub async fn count_table(
    pool: &AnyPool,
    backend: DatabaseBackend,
    table: &'static str,
) -> Option<i64> {
    let sql = adapt_sql(&format!("SELECT COUNT(*) FROM {table}"), backend);
    match crate::db::query_as::<(i64,)>(&sql).fetch_one(pool).await {
        Ok((n,)) => Some(n),
        Err(e) => {
            tracing::debug!(table, error = %e, "telemetry could not count table");
            None
        }
    }
}

/// Run a count, treating any failure as zero.
pub async fn count(pool: &AnyPool, backend: DatabaseBackend, sql: &str, bind: Option<&str>) -> i64 {
    let adapted = adapt_sql(sql, backend);
    let q = crate::db::query_as::<(i64,)>(&adapted);
    let q = match bind {
        Some(v) => q.bind(v),
        None => q,
    };
    match q.fetch_one(pool).await {
        Ok((n,)) => n,
        Err(e) => {
            tracing::warn!(error = %e, "telemetry feature probe failed, reporting unused");
            0
        }
    }
}

/// The start of the active window, RFC 3339 to the second.
pub fn window_start() -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(ACTIVE_WINDOW_DAYS);
    cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}
