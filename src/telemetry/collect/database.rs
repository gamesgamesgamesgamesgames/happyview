//! Counters derived from database state.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::telemetry::payload;

/// Tables counted into `totals`, as (payload key, table name).
const COUNTED_TABLES: &[(&str, &str)] = &[
    ("records", "happyview_records"),
    ("space_records", "happyview_space_records"),
    ("event_logs", "happyview_event_logs"),
    ("space_record_oplog", "happyview_space_record_oplog"),
    ("backfill_repos", "happyview_backfill_repos"),
    ("jobs", "happyview_jobs"),
    ("lexicons", "happyview_lexicons"),
    ("spaces", "happyview_spaces"),
    ("space_members", "happyview_space_members"),
    ("linked_repos", "happyview_linked_repos"),
    ("users", "happyview_users"),
    ("dpop_sessions", "happyview_dpop_sessions"),
];

/// How many collections appear in `top_collection_shares`.
const TOP_COLLECTIONS: usize = 10;

async fn count_table(pool: &AnyPool, backend: DatabaseBackend, table: &'static str) -> Option<i64> {
    let sql = adapt_sql(&format!("SELECT COUNT(*) FROM {table}"), backend);
    match crate::db::query_as::<(i64,)>(&sql).fetch_one(pool).await {
        Ok((n,)) => Some(n),
        Err(e) => {
            tracing::debug!(table, error = %e, "telemetry could not count table");
            None
        }
    }
}

pub async fn totals(pool: &AnyPool, backend: DatabaseBackend) -> payload::Counters {
    let mut out = payload::Counters::new();
    for (key, table) in COUNTED_TABLES {
        if let Some(n) = count_table(pool, backend, table).await {
            out.insert((*key).to_string(), n);
        }
    }
    out
}

/// Total on-disk size of the Postgres database, in bytes.
pub async fn postgres_db_bytes(pool: &AnyPool, backend: DatabaseBackend) -> Option<i64> {
    if backend != DatabaseBackend::Postgres {
        return None;
    }
    let sql = adapt_sql("SELECT pg_database_size(current_database())", backend);
    match crate::db::query_as::<(i64,)>(&sql).fetch_one(pool).await {
        Ok((n,)) => Some(n),
        Err(e) => {
            tracing::warn!(error = %e, "telemetry could not measure postgres database size");
            None
        }
    }
}

/// Summed wall-clock duration of completed jobs, in seconds — `SUM
/// (completed_at - started_at)` over `happyview_jobs`.
pub async fn job_runtime_seconds(pool: &AnyPool, backend: DatabaseBackend) -> Option<i64> {
    let sql = match backend {
        DatabaseBackend::Sqlite => {
            "SELECT CAST(SUM(ROUND((julianday(completed_at) - julianday(started_at)) * 86400)) AS INTEGER) \
             FROM happyview_jobs WHERE completed_at IS NOT NULL AND started_at IS NOT NULL"
        }
        DatabaseBackend::Postgres => {
            "SELECT CAST(SUM(ROUND(EXTRACT(EPOCH FROM (completed_at::timestamptz - started_at::timestamptz))::numeric)) AS BIGINT) \
             FROM happyview_jobs WHERE completed_at IS NOT NULL AND started_at IS NOT NULL"
        }
    };
    match crate::db::query_as::<(Option<i64>,)>(sql)
        .fetch_one(pool)
        .await
    {
        Ok((Some(n),)) => Some(n),
        Ok((None,)) => Some(0),
        Err(e) => {
            tracing::warn!(error = %e, "telemetry could not measure job runtime");
            None
        }
    }
}

async fn collection_counts(pool: &AnyPool, backend: DatabaseBackend) -> Vec<(String, i64)> {
    let sql = adapt_sql(
        "SELECT collection, COUNT(*) AS cnt FROM happyview_records
         GROUP BY collection ORDER BY cnt DESC",
        backend,
    );
    crate::db::query_as::<(String, i64)>(&sql)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
}

/// Row-count share of the largest collections, as percentages, largest first.
pub async fn collection_shares(pool: &AnyPool, backend: DatabaseBackend) -> Vec<f32> {
    let counts = collection_counts(pool, backend).await;
    let total: i64 = counts.iter().map(|(_, n)| *n).sum();
    if total == 0 {
        return Vec::new();
    }
    counts
        .into_iter()
        .take(TOP_COLLECTIONS)
        .map(|(_, n)| (n as f64 / total as f64 * 100.0) as f32)
        .collect()
}

pub async fn collection_names(pool: &AnyPool, backend: DatabaseBackend) -> Vec<String> {
    collection_counts(pool, backend)
        .await
        .into_iter()
        .take(TOP_COLLECTIONS)
        .map(|(name, _)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::test_support::memory_pool;

    async fn pool_with_records() -> sqlx::AnyPool {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_records (
                uri TEXT PRIMARY KEY,
                did TEXT NOT NULL,
                collection TEXT NOT NULL,
                rkey TEXT NOT NULL,
                record TEXT NOT NULL,
                cid TEXT NOT NULL,
                indexed_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .expect("create records");
        pool
    }

    async fn insert_record(pool: &sqlx::AnyPool, uri: &str, collection: &str) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_records
               (uri, did, collection, rkey, record, cid, indexed_at, created_at)
             VALUES (?, ?, ?, ?, '{}', 'bafy', ?, ?)",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind(uri)
            .bind("did:plc:test")
            .bind(collection)
            .bind(uri.rsplit('/').next().unwrap())
            .bind(crate::db::now_rfc3339())
            .bind(crate::db::now_rfc3339())
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn totals_reports_zero_on_an_empty_database() {
        let pool = pool_with_records().await;
        let totals = totals(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(totals.get("records"), Some(&0));
    }

    #[tokio::test]
    async fn totals_counts_records() {
        let pool = pool_with_records().await;
        for i in 0..3 {
            insert_record(
                &pool,
                &format!("at://did:plc:test/app.bsky.feed.post/{i}"),
                "app.bsky.feed.post",
            )
            .await;
        }
        let totals = totals(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(totals.get("records"), Some(&3));
    }

    #[tokio::test]
    async fn collection_shares_are_percentages_largest_first() {
        let pool = pool_with_records().await;
        for i in 0..9 {
            insert_record(
                &pool,
                &format!("at://did:plc:test/app.bsky.graph.follow/{i}"),
                "app.bsky.graph.follow",
            )
            .await;
        }
        insert_record(
            &pool,
            "at://did:plc:test/app.bsky.feed.post/x",
            "app.bsky.feed.post",
        )
        .await;

        let shares = collection_shares(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(shares.len(), 2);
        assert!((shares[0] - 90.0).abs() < 0.01, "got {:?}", shares);
        assert!((shares[1] - 10.0).abs() < 0.01, "got {:?}", shares);
    }

    #[tokio::test]
    async fn collection_shares_are_empty_rather_than_nan_when_there_are_no_records() {
        let pool = pool_with_records().await;
        assert!(
            collection_shares(&pool, DatabaseBackend::Sqlite)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn postgres_db_bytes_is_none_on_sqlite() {
        let pool = pool_with_records().await;
        assert_eq!(
            postgres_db_bytes(&pool, DatabaseBackend::Sqlite).await,
            None
        );
    }

    async fn pool_with_jobs() -> sqlx::AnyPool {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_jobs (
                id            TEXT PRIMARY KEY,
                job_type      TEXT NOT NULL,
                status        TEXT NOT NULL DEFAULT 'pending',
                input         TEXT NOT NULL DEFAULT '{}',
                progress      TEXT NOT NULL DEFAULT '{}',
                result        TEXT,
                error         TEXT,
                created_by    TEXT NOT NULL,
                started_at    TEXT,
                completed_at  TEXT,
                created_at    TEXT NOT NULL DEFAULT (datetime('now')),
                inherit_auth  BOOLEAN NOT NULL DEFAULT 0,
                api_client_id TEXT,
                dpop_key_id   TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create jobs");
        pool
    }

    async fn insert_job(
        pool: &sqlx::AnyPool,
        id: &str,
        status: &str,
        started_at: Option<&str>,
        completed_at: Option<&str>,
    ) {
        crate::db::query(
            "INSERT INTO happyview_jobs
               (id, job_type, status, created_by, started_at, completed_at, created_at)
             VALUES (?, 'test.job', ?, 'did:plc:test', ?, ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(started_at)
        .bind(completed_at)
        .bind(crate::db::now_rfc3339())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn job_runtime_seconds_is_zero_not_none_with_no_completed_jobs() {
        let pool = pool_with_jobs().await;
        assert_eq!(
            job_runtime_seconds(&pool, DatabaseBackend::Sqlite).await,
            Some(0)
        );
    }

    #[tokio::test]
    async fn job_runtime_seconds_sums_completed_job_durations() {
        let pool = pool_with_jobs().await;
        insert_job(
            &pool,
            "job-1",
            "completed",
            Some("2026-01-01T00:00:00+00:00"),
            Some("2026-01-01T00:01:40+00:00"), // 100s
        )
        .await;
        insert_job(
            &pool,
            "job-2",
            "completed",
            Some("2026-01-01T00:00:00+00:00"),
            Some("2026-01-01T01:00:00+00:00"), // 3600s
        )
        .await;
        insert_job(
            &pool,
            "job-3",
            "running",
            Some("2026-01-01T00:00:00+00:00"),
            None,
        )
        .await;

        let total = job_runtime_seconds(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(total, Some(3700));
    }

    #[tokio::test]
    async fn collection_names_are_a_separate_call_from_shares() {
        let pool = pool_with_records().await;
        insert_record(
            &pool,
            "at://did:plc:test/app.bsky.feed.post/x",
            "app.bsky.feed.post",
        )
        .await;

        let names = collection_names(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(names, vec!["app.bsky.feed.post".to_string()]);
    }
}
