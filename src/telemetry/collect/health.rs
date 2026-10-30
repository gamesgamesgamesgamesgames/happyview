//! Operational health and configuration drift.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, adapt_sql, now_rfc3339};
use crate::telemetry::payload;

pub const KEY_RESTART_COUNT: &str = "telemetry.restart_count";
const KEY_VERSION: &str = "telemetry.version_seen";
const KEY_VERSION_SINCE: &str = "telemetry.version_seen_at";

const TOP_EVENT_TYPES: usize = 25;

async fn read(pool: &AnyPool, backend: DatabaseBackend, key: &str) -> Option<String> {
    crate::admin::settings::get_setting(pool, key, backend).await
}

async fn write(pool: &AnyPool, backend: DatabaseBackend, key: &str, value: &str) {
    let sql = adapt_sql(
        "INSERT INTO happyview_instance_settings (key, value, updated_at)
         VALUES (?, ?, ?)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
        backend,
    );
    if let Err(e) = crate::db::query(&sql)
        .bind(key)
        .bind(value)
        .bind(now_rfc3339())
        .execute(pool)
        .await
    {
        tracing::debug!(key, error = %e, "telemetry could not persist health state");
    }
}

/// Increment and return the lifetime restart count.
pub async fn note_restart(pool: &AnyPool, backend: DatabaseBackend) -> i64 {
    let next = read(pool, backend, KEY_RESTART_COUNT)
        .await
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    write(pool, backend, KEY_RESTART_COUNT, &next.to_string()).await;
    next
}

/// Record the version this instance is running, if it has changed. Called at
/// boot, beside [`note_restart`].
///
/// Separate from [`time_on_version`] because assembling a telemetry snapshot
/// must not write: `GET /admin/settings/telemetry/preview` assembles one, and
/// a preview that advanced this marker both wrote from a `GET` the CSRF guard
/// exempts and consumed the signal it was previewing — the next real report
/// would then date this version from whenever someone happened to look.
pub async fn note_version(pool: &AnyPool, backend: DatabaseBackend, version: &str) {
    if read(pool, backend, KEY_VERSION).await.as_deref() == Some(version) {
        return;
    }
    write(pool, backend, KEY_VERSION, version).await;
    write(pool, backend, KEY_VERSION_SINCE, &now_rfc3339()).await;
}

/// When this instance first booted the version it is running now, or `None`
/// while no boot has recorded this version yet. Reads only.
pub async fn time_on_version(
    pool: &AnyPool,
    backend: DatabaseBackend,
    version: &str,
) -> Option<String> {
    if read(pool, backend, KEY_VERSION).await.as_deref() != Some(version) {
        return None;
    }
    read(pool, backend, KEY_VERSION_SINCE).await
}

/// Names of every setting key present in the database.
/// Setting keys HappyView writes itself, beyond those with an environment
/// fallback. `PUT /admin/settings/{key}` accepts any key, so on a managed
/// instance only these are reported by name; see [`is_known_setting_key`].
const INTERNAL_SETTING_KEYS: &[&str] = &[
    crate::feature_flags::FeatureFlag::SPACES_PDS_MIGRATION,
    crate::admin::default_plugins::APPLIED_KEY,
    "happyview.require.loaded",
    "jetstream_cursor",
    "jetstream_host",
    "jetstream_seq",
    "rate_limit.default_procedure_cost",
    "rate_limit.default_proxy_cost",
    "rate_limit.default_query_cost",
    "record_refs_rebuild_cursor",
    "record_refs_rebuilt",
    "record_store_copy_records_after",
    "record_store_copy_refs_after",
    "space_cid_backfill_completed_at",
    "space_commit_format_rebuild_completed_at",
    "sqlite_vacuum_attempt_started_at",
    "sqlite_vacuum_completed_at",
    "sqlite_vacuum_last_result",
    "sqlite_vacuum_requested_at",
    "xrpc_proxy_config",
];

/// Whether HappyView itself names `key`, as opposed to an operator who
/// stored it through the settings API under a name of their choosing.
pub fn is_known_setting_key(key: &str) -> bool {
    crate::admin::settings::env_var_for(key).is_some() || INTERNAL_SETTING_KEYS.contains(&key)
}

pub async fn configured_keys(pool: &AnyPool, backend: DatabaseBackend) -> Vec<String> {
    let sql = adapt_sql(
        "SELECT key FROM happyview_instance_settings ORDER BY key",
        backend,
    );
    crate::db::query_as::<(String,)>(&sql)
        .fetch_all(pool)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(k,)| k)
        .filter(|k| !k.starts_with("telemetry."))
        .collect()
}

/// Event-log volume by type, as `events.<type>` keys.
pub async fn counters(pool: &AnyPool, backend: DatabaseBackend) -> payload::Counters {
    let sql = adapt_sql(
        "SELECT event_type, COUNT(*) AS cnt FROM happyview_event_logs
         GROUP BY event_type ORDER BY cnt DESC",
        backend,
    );
    let rows: Vec<(String, i64)> = match crate::db::query_as(&sql).fetch_all(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "telemetry could not read event log counters");
            return payload::Counters::new();
        }
    };

    rows.into_iter()
        .take(TOP_EVENT_TYPES)
        .map(|(kind, n)| (format!("events.{kind}"), n))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::test_support::memory_pool;

    async fn pool_for_health() -> sqlx::AnyPool {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_instance_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create instance_settings");
        crate::db::query(
            "CREATE TABLE happyview_event_logs (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                severity TEXT NOT NULL DEFAULT 'info',
                actor_did TEXT,
                subject TEXT,
                detail TEXT NOT NULL DEFAULT '{}',
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create event_logs");
        pool
    }

    async fn set(pool: &sqlx::AnyPool, key: &str, value: &str) {
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_instance_settings (key, value, updated_at) VALUES (?, ?, ?)",
            DatabaseBackend::Sqlite,
        );
        crate::db::query(&sql)
            .bind(key)
            .bind(value)
            .bind(crate::db::now_rfc3339())
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restart_count_starts_at_one_and_increments() {
        let pool = pool_for_health().await;
        assert_eq!(note_restart(&pool, DatabaseBackend::Sqlite).await, 1);
        assert_eq!(note_restart(&pool, DatabaseBackend::Sqlite).await, 2);
        assert_eq!(note_restart(&pool, DatabaseBackend::Sqlite).await, 3);
    }

    #[tokio::test]
    async fn configured_keys_reports_names_and_never_values() {
        let pool = pool_for_health().await;
        set(&pool, "app_name", "My AppView").await;
        set(&pool, "some_secret", "hunter2").await;

        let keys = configured_keys(&pool, DatabaseBackend::Sqlite).await;
        assert!(keys.contains(&"app_name".to_string()));
        assert!(keys.contains(&"some_secret".to_string()));

        let encoded = serde_json::to_string(&keys).unwrap();
        assert!(!encoded.contains("hunter2"));
        assert!(!encoded.contains("My AppView"));
    }

    #[tokio::test]
    async fn telemetry_own_keys_are_excluded_from_configured_keys() {
        let pool = pool_for_health().await;
        set(&pool, "telemetry.mode", "auto").await;
        set(&pool, "app_name", "x").await;

        let keys = configured_keys(&pool, DatabaseBackend::Sqlite).await;
        assert!(!keys.iter().any(|k| k.starts_with("telemetry.")));
        assert!(keys.contains(&"app_name".to_string()));
    }

    async fn settings_count(pool: &AnyPool) -> i64 {
        let (n,): (i64,) = crate::db::query_as("SELECT count(*) FROM happyview_instance_settings")
            .fetch_one(pool)
            .await
            .unwrap();
        n
    }

    #[tokio::test]
    async fn time_on_version_is_none_until_a_boot_records_the_version() {
        let pool = pool_for_health().await;
        assert!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0")
                .await
                .is_none()
        );
        note_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await;
        assert!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0")
                .await
                .is_some()
        );
    }

    /// Asking how long this version has been running records nothing.
    ///
    /// `GET /admin/settings/telemetry/preview` assembles a snapshot, and this
    /// is what it reaches. A write here is a write from a `GET` the CSRF guard
    /// exempts, and it also consumes the signal the preview is showing.
    #[tokio::test]
    async fn reading_the_version_stamp_writes_nothing() {
        let pool = pool_for_health().await;
        let before = settings_count(&pool).await;
        for _ in 0..3 {
            let _ = time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await;
        }
        assert_eq!(settings_count(&pool).await, before);
    }

    #[tokio::test]
    async fn upgrading_resets_the_version_stamp() {
        let pool = pool_for_health().await;
        note_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await;
        assert!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0")
                .await
                .is_some()
        );
        // The new version has no stamp until its own boot records one.
        assert!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.2.0")
                .await
                .is_none()
        );
        note_version(&pool, DatabaseBackend::Sqlite, "0.2.0").await;
        assert!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.2.0")
                .await
                .is_some()
        );
    }

    /// A second boot of the same version keeps the first one's stamp, or
    /// "time on this version" would reset at every restart.
    #[tokio::test]
    async fn noting_the_same_version_twice_keeps_the_first_stamp() {
        let pool = pool_for_health().await;
        note_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await;
        let first = time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0")
            .await
            .expect("the first boot should stamp it");
        note_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await;
        assert_eq!(
            time_on_version(&pool, DatabaseBackend::Sqlite, "0.1.0").await,
            Some(first)
        );
    }

    #[tokio::test]
    async fn counters_report_event_log_volume_by_type() {
        let pool = pool_for_health().await;
        let sql = crate::db::adapt_sql(
            "INSERT INTO happyview_event_logs (id, event_type, severity, detail, created_at)
             VALUES (?, ?, 'error', '{}', ?)",
            DatabaseBackend::Sqlite,
        );
        for i in 0..4 {
            crate::db::query(&sql)
                .bind(format!("evt-{i}"))
                .bind("job.failed")
                .bind(crate::db::now_rfc3339())
                .execute(&pool)
                .await
                .unwrap();
        }

        let counters = counters(&pool, DatabaseBackend::Sqlite).await;
        assert_eq!(counters.get("events.job.failed"), Some(&4));
    }

    #[tokio::test]
    async fn counters_survive_a_missing_event_log_table() {
        let pool = pool_for_health().await;
        crate::db::query("DROP TABLE happyview_event_logs")
            .execute(&pool)
            .await
            .unwrap();

        let result = counters(&pool, DatabaseBackend::Sqlite).await;
        assert!(
            result.is_empty(),
            "a missing table must yield an empty map, not garbage"
        );
    }
}
