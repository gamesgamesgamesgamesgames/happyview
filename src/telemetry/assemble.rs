//! Snapshot assembly.

use crate::db::now_rfc3339;
use crate::telemetry::collect::{database, features, health, host, lexicons as lexicon_detail};
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

/// A record source's metrics: `feature.<name>.ever` and
/// `feature.<name>.active` (non-zero for true) go to `features`, anything
/// else to `totals`.
fn add_source_metrics(
    totals: &mut crate::telemetry::payload::Counters,
    features: &mut std::collections::BTreeMap<String, crate::telemetry::payload::FeatureUsage>,
    metrics: Vec<(String, i64)>,
) {
    for (key, value) in metrics {
        let feature = key.strip_prefix("feature.").and_then(|rest| {
            rest.strip_suffix(".ever")
                .map(|name| (name, true))
                .or_else(|| rest.strip_suffix(".active").map(|name| (name, false)))
        });
        match feature {
            Some((name, ever)) => {
                let usage = features.entry(name.to_string()).or_default();
                if ever {
                    usage.ever = value != 0;
                } else {
                    usage.active = value != 0;
                }
            }
            None => {
                totals.insert(key, value);
            }
        }
    }
}

pub async fn assemble(
    records: &crate::record_store::RecordStore,
    db_url: &str,
    consent: &Consent,
    counters: &Counters,
    lexicons: &crate::catalog::LexiconCatalog,
    source: &dyn happyview_provider::RecordSource,
) -> Option<Snapshot> {
    let instance_id = consent.instance_id.clone()?;
    let (pool, backend) = (records.db(), records.backend());

    let totals_fut = async {
        budgeted("totals", database::totals(records))
            .await
            .unwrap_or_default()
    };
    let features_fut = async {
        budgeted("features", features::usage(pool, backend))
            .await
            .unwrap_or_default()
    };
    let shares_fut = async {
        budgeted("collection_shares", database::collection_shares(records))
            .await
            .unwrap_or_default()
    };
    let names_fut = async {
        if consent.lexicon_names {
            budgeted("collection_names", database::collection_names(records)).await
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
    // Through the catalog rather than the table, since a project's lexicons
    // have no rows.
    let lexicon_list_fut = async {
        match budgeted("lexicons", lexicons.list(pool, backend)).await {
            Some(Ok(listed)) => Some(listed),
            Some(Err(e)) => {
                tracing::warn!(metric = "lexicons", error = %e, "telemetry metric failed, omitting");
                None
            }
            None => None,
        }
    };
    let source_metrics_fut = async {
        budgeted("source_metrics", source.metrics())
            .await
            .unwrap_or_default()
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
        lexicon_list,
        source_metrics,
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
        lexicon_list_fut,
        source_metrics_fut,
    );

    let mut totals = totals;
    let mut feature_usage = feature_usage;
    add_source_metrics(&mut totals, &mut feature_usage, source_metrics);
    totals.extend(health_counters);
    if let Some(count) = restart_count {
        totals.insert("restarts".to_string(), count);
    }
    if let Some(secs) = job_runtime_seconds {
        totals.insert("job_runtime_seconds".to_string(), secs);
    }

    let mut host_report = host_report;
    if let Some(keys) = configured_keys {
        // An operator may name a setting anything, so such keys are reported
        // only as a count.
        let (keys, other): (Vec<_>, Vec<_>) = keys
            .into_iter()
            .partition(|k| health::is_known_setting_key(k));
        host_report.insert(
            "configured_setting_keys".to_string(),
            serde_json::json!(keys),
        );
        host_report.insert(
            "configured_other_setting_keys".to_string(),
            serde_json::json!(other.len()),
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

    let docs: Vec<&serde_json::Value> = lexicon_list
        .iter()
        .flatten()
        .map(|l| &l.lexicon_json)
        .collect();
    let structures = consent
        .lexicon_structure
        .then(|| lexicon_detail::structures(docs.iter().copied()));
    let (documents, documents_omitted) = if consent.lexicon_documents {
        let (kept, omitted) =
            lexicon_detail::documents(docs.iter().copied(), lexicon_detail::DOCUMENTS_BYTE_BUDGET);
        (Some(kept), (omitted > 0).then_some(omitted))
    } else {
        (None, None)
    };
    let lexicon_count = lexicon_list
        .as_ref()
        .map(|listed| i64::try_from(listed.len()).unwrap_or(i64::MAX))
        .or_else(|| totals.get("lexicons").copied())
        .unwrap_or(0);
    if lexicon_count > 0 {
        feature_usage
            .entry("lexicons".to_string())
            .or_default()
            .ever = true;
    }

    Some(Snapshot {
        schema_version: SCHEMA_VERSION,
        instance_id,
        reported_at: now_rfc3339(),
        report_mode: consent.mode.as_str().to_string(),
        happyview_version: crate::version::version().to_string(),
        process_started_at: counters.process_started_at().to_string(),
        contact: consent.contact.clone(),
        managed: consent.managed_by.clone(),
        totals,
        since_start: counters.snapshot(),
        features: feature_usage,
        host: host_report,
        lexicons: LexiconReport {
            count: u32::try_from(lexicon_count).unwrap_or(u32::MAX),
            top_collection_shares: shares,
            names,
            structures,
            documents,
            documents_omitted,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::LexiconCatalog;
    use crate::db::DatabaseBackend;
    use crate::telemetry::consent::{Consent, TelemetryMode};
    use crate::telemetry::counters::Counters;
    use crate::test_support::migrated_memory_pool;

    async fn pool_for_assembly() -> sqlx::AnyPool {
        migrated_memory_pool().await
    }

    fn consent(mode: TelemetryMode) -> Consent {
        Consent {
            mode,
            contact: None,
            lexicon_names: false,
            lexicon_structure: false,
            lexicon_documents: false,
            instance_id: Some("11111111-2222-3333-4444-555555555555".into()),
            prompted: true,
            managed_by: None,
        }
    }

    #[tokio::test]
    async fn returns_none_without_an_instance_id() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.instance_id = None;

        let out = assemble(
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &c,
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
        )
        .await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn builds_a_snapshot_with_shape_data_and_no_consented_extras() {
        let pool = pool_for_assembly().await;
        let snap = assemble(
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
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
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &consent(TelemetryMode::Manual),
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
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
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &c,
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
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
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &c,
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
        )
        .await
        .unwrap();
        assert!(snap.lexicons.names.is_some());
        assert!(
            snap.lexicons.structures.is_none(),
            "names must not imply structure"
        );
    }

    async fn put_setting(pool: &sqlx::AnyPool, key: &str, value: &str) {
        crate::admin::settings::put_setting(pool, key, value, DatabaseBackend::Sqlite)
            .await
            .unwrap();
    }

    async fn assemble_with(pool: &sqlx::AnyPool, c: &Consent) -> Snapshot {
        assemble_in(pool, c, &LexiconCatalog::detached()).await
    }

    async fn assemble_in(pool: &sqlx::AnyPool, c: &Consent, catalog: &LexiconCatalog) -> Snapshot {
        assemble(
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            c,
            &Counters::new(),
            catalog,
            &*happyview_source_jetstream::JetstreamSource::detached(),
        )
        .await
        .unwrap()
    }

    /// Every string in `value`, keys included.
    fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    out.push(k.clone());
                    strings(v, out);
                }
            }
            _ => {}
        }
    }

    #[tokio::test]
    async fn a_managed_snapshot_carries_only_instance_level_data() {
        let pool = pool_for_assembly().await;
        crate::db::query(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at)
             VALUES ('at://did:plc:alice/com.example.post/1', 'did:plc:alice', 'com.example.post', '1',
                     '{\"text\":\"hello from alice\"}', 'bafy', '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00')",
        )
        .execute(&pool)
        .await
        .unwrap();
        put_setting(&pool, "app_name", "Example").await;
        put_setting(&pool, "alice@example.com", "x").await;

        let mut c = consent(TelemetryMode::Off);
        c.contact = Some("operator@example.com".into());
        c.lexicon_names = true;
        c.lexicon_structure = true;
        c.lexicon_documents = true;
        let snap = assemble_in(&pool, &c.managed_by(Some("acme")), &project_catalog()).await;

        assert_eq!(snap.managed.as_deref(), Some("acme"));
        assert_eq!(snap.report_mode, "auto");
        assert!(snap.contact.is_none());
        assert!(snap.lexicons.names.is_none());
        assert!(snap.lexicons.structures.is_none());
        assert!(snap.lexicons.documents.is_none());
        assert_eq!(snap.lexicons.count, 1);
        assert_eq!(
            snap.host["configured_setting_keys"],
            serde_json::json!(["app_name"])
        );
        assert_eq!(snap.host["configured_other_setting_keys"], 1);
        assert_eq!(snap.totals["records"], 1);

        let mut seen = Vec::new();
        strings(&serde_json::to_value(&snap).unwrap(), &mut seen);
        for s in &seen {
            assert!(
                !s.contains('@') && !s.contains("did:") && !s.contains("alice"),
                "a managed snapshot carried {s:?}"
            );
        }
    }

    /// An operator names a setting stored through the API, so its name could
    /// identify them; it is counted, managed or not.
    #[tokio::test]
    async fn an_unmanaged_snapshot_counts_custom_setting_keys() {
        let pool = pool_for_assembly().await;
        put_setting(&pool, "app_name", "Example").await;
        put_setting(&pool, "my_custom_key", "x").await;

        let snap = assemble_with(&pool, &consent(TelemetryMode::Auto)).await;

        assert!(snap.managed.is_none());
        assert_eq!(
            snap.host["configured_setting_keys"],
            serde_json::json!(["app_name"])
        );
        assert_eq!(snap.host["configured_other_setting_keys"], 1);
        assert!(
            !serde_json::to_string(&snap)
                .unwrap()
                .contains("my_custom_key")
        );
    }

    #[tokio::test]
    async fn lexicon_structure_appears_only_when_consented() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.lexicon_structure = true;

        let snap = assemble_in(&pool, &c, &project_catalog()).await;

        let structures = snap.lexicons.structures.expect("structure was consented");
        assert_eq!(structures.len(), 1);
        assert_eq!(structures[0]["main_type"], "record");
        assert!(
            snap.lexicons.names.is_none(),
            "structure must not imply names"
        );
        assert!(snap.lexicons.documents.is_none());
        assert!(
            !serde_json::to_string(&structures)
                .unwrap()
                .contains("com.example"),
            "a structure carried the NSID"
        );
    }

    #[tokio::test]
    async fn lexicon_documents_appear_only_when_consented() {
        let pool = pool_for_assembly().await;
        let mut c = consent(TelemetryMode::Auto);
        c.lexicon_documents = true;

        let snap = assemble_in(&pool, &c, &project_catalog()).await;

        let documents = snap.lexicons.documents.expect("documents were consented");
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0]["id"], "com.example.project.thing");
        assert_eq!(snap.lexicons.documents_omitted, None);
        assert!(snap.lexicons.structures.is_none());
        assert!(snap.lexicons.names.is_none());
    }

    #[tokio::test]
    async fn a_broken_table_still_produces_a_snapshot() {
        let pool = pool_for_assembly().await;
        crate::db::query("DROP TABLE happyview_spaces")
            .execute(&pool)
            .await
            .unwrap();

        let snap = assemble(
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
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
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
        )
        .await
        .unwrap();

        assert_eq!(snap.totals.get("restarts"), Some(&recorded));
    }

    #[tokio::test]
    async fn job_runtime_seconds_round_trips_into_totals() {
        let pool = pool_for_assembly().await;
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
            &crate::record_store::RecordStore::new(pool.clone(), DatabaseBackend::Sqlite),
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
            &LexiconCatalog::detached(),
            &*happyview_source_jetstream::JetstreamSource::detached(),
        )
        .await
        .unwrap();

        assert_eq!(snap.totals.get("job_runtime_seconds"), Some(&60));
    }

    /// A catalog holding one project lexicon, `com.example.project.thing`.
    fn project_catalog() -> LexiconCatalog {
        use crate::lexicon::{ParsedLexicon, ProcedureAction};
        use crate::project::snapshot::{ProjectLexicon, ProjectSnapshot};

        let parsed = ParsedLexicon::parse(
            serde_json::json!({
                "lexicon": 1,
                "id": "com.example.project.thing",
                "defs": { "main": { "type": "record", "key": "tid",
                    "record": { "type": "object", "properties": {} } } }
            }),
            0,
            None,
            ProcedureAction::Upsert,
            None,
        )
        .unwrap();
        let mut project = ProjectSnapshot::empty();
        project.lexicons.insert(
            parsed.id.clone(),
            ProjectLexicon {
                parsed,
                path: "lexicons/thing.json".into(),
                backfill: false,
                indexed: true,
            },
        );
        LexiconCatalog::new(std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            project,
        )))
    }

    #[tokio::test]
    async fn a_project_lexicon_is_counted_though_it_has_no_row() {
        let pool = pool_for_assembly().await;
        let snap = assemble_in(&pool, &consent(TelemetryMode::Auto), &project_catalog()).await;

        assert_eq!(snap.lexicons.count, 1);
        assert!(snap.features["lexicons"].ever);
    }

    /// The backfill tables and the `backfill` feature come from the record
    /// source, under the keys the payload has always had.
    #[tokio::test]
    async fn the_record_source_reports_its_tables_and_features() {
        let state = crate::test_support::test_state_with_pool(pool_for_assembly().await);
        crate::db::query(
            "INSERT INTO happyview_backfill_jobs (id, collection, status, created_at) \
             VALUES ('job', 'com.example.thing', 'completed', ?)",
        )
        .bind(crate::db::now_rfc3339())
        .execute(&state.db)
        .await
        .unwrap();

        let snap = assemble(
            &state.records,
            "sqlite::memory:",
            &consent(TelemetryMode::Auto),
            &Counters::new(),
            &LexiconCatalog::detached(),
            state.source.as_ref(),
        )
        .await
        .unwrap();

        for key in [
            "backfill_repos",
            "backfill_queue",
            "backfill_completions",
            "backfill_pds_stats",
            "backfill_cursors",
        ] {
            assert_eq!(snap.totals.get(key), Some(&0), "{key}");
        }
        assert!(!snap.totals.keys().any(|k| k.starts_with("feature.")));
        let backfill = snap.features["backfill"];
        assert!(backfill.ever && backfill.active);
    }
}
