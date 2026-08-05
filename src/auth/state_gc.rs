//! Reaper for expired OAuth-flow state rows.
//!
//! Two tables accumulate short-lived rows that are only ever cleared on the
//! happy path — when the flow they belong to completes:
//!
//! - `happyview_auth_login_redirects` — dashboard-login redirect targets
//! - `happyview_linked_repo_auth_state` — linked-repo invites and in-flight
//!   authorizations
//!
//! Abandoned flows leave rows behind in both. Nothing stale is ever *honoured*
//! (every read filters on `expires_at`), so this is unbounded growth of inert
//! rows rather than a correctness problem — but on a busy instance it is growth
//! with no ceiling, so it gets swept.
//!
//! They share a reaper because they share a shape: a TEXT `expires_at` holding
//! an RFC3339 timestamp with a `+00:00` offset. Comparison is lexicographic,
//! which is only correct because every writer uses that same offset form rather
//! than a `Z` suffix — `'+' < 'Z'`, so a mix would silently invert the compare.
//! See the note on `linked_repos::flow::expires_at`.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};

/// Tables swept, in the order they are swept.
const EXPIRING_STATE_TABLES: &[&str] = &[
    "happyview_auth_login_redirects",
    "happyview_linked_repo_auth_state",
];

/// Delete every row whose `expires_at` has passed. Returns rows removed.
///
/// `table` is a `&'static str` from the list above and never derived from user
/// input, so interpolating it is not an injection vector.
async fn purge_expired(pool: &AnyPool, backend: DatabaseBackend, table: &'static str) -> u64 {
    let sql = adapt_sql(
        &format!("DELETE FROM {table} WHERE expires_at < ?"),
        backend,
    );
    match crate::db::query(&sql)
        .bind(now_rfc3339())
        .execute(pool)
        .await
    {
        Ok(result) => result.rows_affected(),
        Err(e) => {
            tracing::warn!(table, error = %e, "failed to purge expired state rows");
            0
        }
    }
}

/// Sweep once. Exposed so tests can drive a pass without the loop.
pub async fn purge_expired_state(pool: &AnyPool, backend: DatabaseBackend) -> u64 {
    let mut total = 0;
    for table in EXPIRING_STATE_TABLES {
        let removed = purge_expired(pool, backend, table).await;
        if removed > 0 {
            tracing::debug!(table, removed, "purged expired state rows");
        }
        total += removed;
    }
    total
}

/// Background loop. Spawned once from `main`.
pub async fn run_expired_state_gc(pool: AnyPool, backend: DatabaseBackend) {
    tracing::info!("starting expired OAuth state cleanup task");
    let interval = tokio::time::Duration::from_secs(3600); // 1 hour

    loop {
        tokio::time::sleep(interval).await;
        let removed = purge_expired_state(&pool, backend).await;
        if removed > 0 {
            tracing::info!(removed, "purged expired OAuth state rows");
        }
    }
}
