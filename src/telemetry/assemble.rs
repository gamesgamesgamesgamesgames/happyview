//! Snapshot assembly.

use sqlx::AnyPool;

use crate::db::{DatabaseBackend, now_rfc3339};
use crate::telemetry::collect::{database, features, health, host};
use crate::telemetry::consent::Consent;
use crate::telemetry::counters::Counters;
use crate::telemetry::payload::{LexiconReport, SCHEMA_VERSION, Snapshot};

const METRIC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

async fn budgeted<F, T>(name: &'static str, fut: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(METRIC_TIMEOUT, fut).await {
        Ok(v) => Some(v),
        Err(_) => {
            tracing::warn!(metric = name, "telemetry metric timed out, omitting");
            None
        }
    }
}

pub async fn assemble(
    pool: &AnyPool,
    backend: DatabaseBackend,
    db_url: &str,
    consent: &Consent,
    counters: &Counters,
) -> Option<Snapshot> {
    let instance_id = consent.instance_id.clone()?;

    let totals_fut = async {
        budgeted("totals", database::totals(pool, backend))
            .await
            .unwrap_or_default()
    };
    let features_fut = async {
        budgeted("features", features::usage(pool, backend))
            .await
            .unwrap_or_default()
    };
    let shares_fut = async {
        budgeted(
            "collection_shares",
            database::collection_shares(pool, backend),
        )
        .await
        .unwrap_or_default()
    };
    let names_fut = async {
        if consent.lexicon_names {
            budgeted(
                "collection_names",
                database::collection_names(pool, backend),
            )
            .await
        } else {
            None
        }
    };
    let host_fut = async {
        let db_url_owned = db_url.to_string();
        match budgeted(
            "host",
            tokio::task::spawn_blocking(move || host::report(&db_url_owned)),
        )
        .await
        {
            Some(Ok(report)) => Some(report),
            Some(Err(e)) => {
                tracing::warn!(metric = "host", error = %e, "telemetry metric panicked, omitting");
                None
            }
            None => None,
        }
        .unwrap_or_default()
    };

    let health_counters_fut = async {
        budgeted("health_counters", health::counters(pool, backend))
            .await
            .unwrap_or_default()
    };
    let configured_keys_fut =
        async { budgeted("configured_keys", health::configured_keys(pool, backend)).await };
    let version_since_fut = async {
        budgeted(
            "version_since",
            health::time_on_version(pool, backend, crate::version::version()),
        )
        .await
        .flatten()
    };
    let restart_count_fut = async {
        budgeted(
            "restart_count",
            crate::admin::settings::get_setting(pool, health::KEY_RESTART_COUNT, backend),
        )
        .await
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
    };
    let vacuum_fut = async {
        budgeted(
            "vacuum",
            crate::maintenance::vacuum::read_status(pool, backend),
        )
        .await
    };
    let postgres_db_bytes_fut = async {
        budgeted(
            "postgres_db_bytes",
            database::postgres_db_bytes(pool, backend),
        )
        .await
        .flatten()
    };
    let job_runtime_seconds_fut = async {
        budgeted(
            "job_runtime_seconds",
            database::job_runtime_seconds(pool, backend),
        )
        .await
        .flatten()
    };

    let (
        totals,
        feature_usage,
        shares,
        names,
        host_report,
        health_counters,
        configured_keys,
        version_since,
        restart_count,
        vacuum_result,
        postgres_db_bytes,
        job_runtime_seconds,
    ) = tokio::join!(
        totals_fut,
        features_fut,
        shares_fut,
        names_fut,
        host_fut,
        health_counters_fut,
        configured_keys_fut,
        version_since_fut,
        restart_count_fut,
        vacuum_fut,
        postgres_db_bytes_fut,
        job_runtime_seconds_fut,
    );

    let mut totals = totals;
    totals.extend(health_counters);
    if let Some(count) = restart_count {
        totals.insert("restarts".to_string(), count);
    }
    if let Some(secs) = job_runtime_seconds {
        totals.insert("job_runtime_seconds".to_string(), secs);
    }

    let mut host_report = host_report;
    if let Some(keys) = configured_keys {
        host_report.insert(
            "configured_setting_keys".to_string(),
            serde_json::json!(keys),
        );
    }
    if let Some(since) = version_since {
        host_report.insert("version_seen_since".to_string(), serde_json::json!(since));
    }
    if let Some(Ok(vacuum)) = vacuum_result {
        host_report.insert("vacuum".to_string(), serde_json::json!(vacuum));
    }
    if let Some(bytes) = postgres_db_bytes {
        host_report
            .entry("db_bytes".to_string())
            .or_insert_with(|| serde_json::json!(bytes));
    }

    let lexicon_count = totals.get("lexicons").copied().unwrap_or(0);

    Some(Snapshot {
        schema_version: SCHEMA_VERSION,
        instance_id,
        reported_at: now_rfc3339(),
        report_mode: consent.mode.as_str().to_string(),
        happyview_version: crate::version::version().to_string(),
        process_started_at: counters.process_started_at().to_string(),
        contact: consent.contact.clone(),
        totals,
        since_start: counters.snapshot(),
        features: feature_usage,
        host: host_report,
        lexicons: LexiconReport {
            count: u32::try_from(lexicon_count).unwrap_or(u32::MAX),
            top_collection_shares: shares,
            names,
            structures: None,
            documents: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseBackend;
    use crate::telemetry::consent::{Consent, TelemetryMode};
    use crate::telemetry::counters::Counters;
    use crate::test_support::memory_pool;

    async fn pool_for_assembly() -> sqlx::AnyPool {
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
        crate::db::query(
            "CREATE TABLE happyview_spaces (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create spaces");
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
        pool
    }

    fn consent(mode: TelemetryMode) -> Consent {
        Consent {
            mode,
            contact: None,
            lexicon_names: false,
            lexicon_structure: false,
            lexicon_documents: false,
            instance_id: Some("11111111-2222-3333-4444-555555555555".into()),
        }
    }

    #[tokio::test]
    async fn returns_none_without_an_instance_id() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.instance_id = None;

        let out = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &c,
            &Counters::new(),
        )
        .await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn builds_a_snapshot_with_shape_data_and_no_consented_extras() {
        let pool = pool_for_assembly().await;
        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
        )
        .await
        .unwrap();

        assert_eq!(
            snap.schema_version,
            crate::telemetry::payload::SCHEMA_VERSION
        );
        assert_eq!(snap.report_mode, "auto");
        assert!(snap.contact.is_none());
        assert!(snap.lexicons.names.is_none());
        assert!(snap.lexicons.structures.is_none());
        assert!(snap.lexicons.documents.is_none());
        assert!(snap.totals.contains_key("records"));
        assert!(snap.since_start.contains_key("jetstream_events_received"));
    }

    #[tokio::test]
    async fn report_mode_reflects_manual() {
        let pool = pool_for_assembly().await;
        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &consent(TelemetryMode::Manual),
            &Counters::new(),
        )
        .await
        .unwrap();

        assert_eq!(snap.report_mode, "manual");
    }

    #[tokio::test]
    async fn contact_appears_only_when_consented() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.contact = Some("tre@trezy.com".into());

        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &c,
            &Counters::new(),
        )
        .await
        .unwrap();
        assert_eq!(snap.contact.as_deref(), Some("tre@trezy.com"));
    }

    #[tokio::test]
    async fn lexicon_names_appear_only_when_consented() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.lexicon_names = true;

        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &c,
            &Counters::new(),
        )
        .await
        .unwrap();
        assert!(snap.lexicons.names.is_some());
        assert!(
            snap.lexicons.structures.is_none(),
            "names must not imply structure"
        );
    }

    #[tokio::test]
    async fn a_broken_table_still_produces_a_snapshot() {
        let pool = pool_for_assembly().await;
        crate::db::query("DROP TABLE happyview_spaces")
            .execute(&pool)
            .await
            .unwrap();

        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
        )
        .await;

        assert!(
            snap.is_some(),
            "one failed metric must not sink the payload"
        );
        assert!(!snap.unwrap().totals.contains_key("spaces"));
    }

    #[tokio::test]
    async fn restart_count_round_trips_into_totals() {
        let pool = pool_for_assembly().await;
        let recorded = health::note_restart(&pool, DatabaseBackend::Sqlite).await;

        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
        )
        .await
        .unwrap();

        assert_eq!(snap.totals.get("restarts"), Some(&recorded));
    }

    #[tokio::test]
    async fn job_runtime_seconds_round_trips_into_totals() {
        let pool = pool_for_assembly().await;
        crate::db::query(
            "CREATE TABLE happyview_jobs (
                id TEXT PRIMARY KEY,
                job_type TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                created_by TEXT NOT NULL,
                started_at TEXT,
                completed_at TEXT,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create jobs");
        crate::db::query(
            "INSERT INTO happyview_jobs
               (id, job_type, status, created_by, started_at, completed_at, created_at)
             VALUES ('job-1', 'test.job', 'completed', 'did:plc:test',
                     '2026-01-01T00:00:00+00:00', '2026-01-01T00:01:00+00:00',
                     '2026-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .expect("insert completed job");

        let snap = assemble(
            &pool,
            DatabaseBackend::Sqlite,
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
        )
        .await
        .unwrap();

        assert_eq!(snap.totals.get("job_runtime_seconds"), Some(&60));
    }
}
