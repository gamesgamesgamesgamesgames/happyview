//! Per-subsystem adoption.

use std::collections::BTreeMap;

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql};
use crate::telemetry::payload::FeatureUsage;

const ACTIVE_WINDOW_DAYS: i64 = 7;

type Probe = (&'static str, &'static str, &'static str, i64);

const PROBES: &[Probe] = &[
    (
        "lexicons",
        "SELECT COUNT(*) FROM happyview_lexicons",
        "SELECT COUNT(*) FROM happyview_lexicons WHERE created_at >= ?",
        1,
    ),
    (
        "backfill",
        "SELECT COUNT(*) FROM happyview_backfill_jobs",
        "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE created_at >= ?",
        1,
    ),
    (
        "scripts",
        "SELECT COUNT(*) FROM happyview_scripts",
        "SELECT COUNT(*) FROM happyview_scripts WHERE updated_at >= ?",
        1,
    ),
    (
        "jobs",
        "SELECT COUNT(*) FROM happyview_jobs",
        "SELECT COUNT(*) FROM happyview_jobs WHERE created_at >= ?",
        10,
    ),
    (
        "plugins",
        "SELECT COUNT(*) FROM happyview_plugins",
        "SELECT COUNT(*) FROM happyview_plugins WHERE enabled = true AND ? IS NOT NULL",
        1,
    ),
    (
        "labelers",
        "SELECT COUNT(*) FROM happyview_labeler_subscriptions",
        "SELECT COUNT(*) FROM happyview_labeler_subscriptions WHERE status = 'active' AND ? IS NOT NULL",
        1,
    ),
    (
        "spaces",
        "SELECT COUNT(*) FROM happyview_spaces",
        "SELECT COUNT(*) FROM happyview_space_record_oplog WHERE created_at >= ?",
        1,
    ),
    (
        "linked_repos",
        "SELECT COUNT(*) FROM happyview_linked_repos",
        "SELECT COUNT(*) FROM happyview_linked_repos WHERE status = 'active' AND ? IS NOT NULL",
        1,
    ),
    (
        "dpop_clients",
        "SELECT COUNT(*) FROM happyview_api_clients",
        "SELECT COUNT(*) FROM happyview_dpop_sessions WHERE ? IS NOT NULL",
        1,
    ),
    (
        "api_keys",
        "SELECT COUNT(*) FROM happyview_api_keys",
        "SELECT COUNT(*) FROM happyview_api_keys WHERE revoked_at IS NULL AND ? IS NOT NULL",
        1,
    ),
];

/// Run a count, treating any failure as zero.
async fn count(pool: &AnyPool, backend: DatabaseBackend, sql: &str, bind: Option<&str>) -> i64 {
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

fn window_start() -> String {
    let cutoff = chrono::Utc::now() - chrono::Duration::days(ACTIVE_WINDOW_DAYS);
    cutoff.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

pub async fn usage(pool: &AnyPool, backend: DatabaseBackend) -> BTreeMap<String, FeatureUsage> {
    let since = window_start();
    let mut out = BTreeMap::new();

    for (key, ever_sql, active_sql, floor) in PROBES {
        let ever = count(pool, backend, ever_sql, None).await >= 1;
        let active = if ever {
            count(pool, backend, active_sql, Some(&since)).await >= *floor
        } else {
            false
        };
        out.insert((*key).to_string(), FeatureUsage { ever, active });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::test_support::memory_pool;

    async fn pool_with_lexicons() -> sqlx::AnyPool {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_lexicons (
                id TEXT PRIMARY KEY,
                revision INTEGER NOT NULL DEFAULT 1,
                lexicon_json TEXT NOT NULL,
                backfill INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            )",
        )
        .execute(&pool)
        .await
        .expect("create lexicons");
        pool
    }

    async fn add_lexicon(pool: &sqlx::AnyPool, nsid: &str, created_at: &str) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_lexicons (id, lexicon_json, backfill, created_at)
             VALUES (?, '{}', 0, ?)",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind(nsid)
            .bind(created_at)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_empty_instance_reports_every_subsystem_unused() {
        let pool = pool_with_lexicons().await;
        let usage = usage(&pool, DatabaseBackend::Sqlite).await;

        assert!(
            !usage.is_empty(),
            "every subsystem must appear, used or not"
        );
        for (name, u) in &usage {
            assert!(!u.ever, "{name} should not be marked ever-used");
            assert!(!u.active, "{name} should not be marked active");
        }
    }

    #[tokio::test]
    async fn a_recently_added_lexicon_is_ever_and_active() {
        let pool = pool_with_lexicons().await;
        add_lexicon(&pool, "com.example.thing", &crate::db::now_rfc3339()).await;

        let usage = usage(&pool, DatabaseBackend::Sqlite).await;
        let lex = usage.get("lexicons").unwrap();
        assert!(lex.ever);
        assert!(lex.active);
    }

    #[tokio::test]
    async fn an_old_lexicon_is_ever_but_not_active() {
        let pool = pool_with_lexicons().await;
        add_lexicon(&pool, "com.example.stale", "2020-01-01T00:00:00+00:00").await;

        let usage = usage(&pool, DatabaseBackend::Sqlite).await;
        let lex = usage.get("lexicons").unwrap();
        assert!(lex.ever);
        assert!(!lex.active);
    }

    #[tokio::test]
    async fn a_missing_table_reports_unused_rather_than_failing() {
        let pool = pool_with_lexicons().await;
        let usage = usage(&pool, DatabaseBackend::Sqlite).await;
        let spaces = usage.get("spaces").unwrap();
        assert!(!spaces.ever, "a missing table must report unused, not used");
        assert!(!spaces.active);
    }
}
