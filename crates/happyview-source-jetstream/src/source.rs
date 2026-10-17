//! `JetstreamSource`: the Jetstream record source behind the `RecordSource`
//! contract. The live consumer and the backfill job system are this crate's
//! own. A gap in the live stream is replayed by the `gap_replay` HappyView
//! hands over, until archive replay moves here too.

use std::sync::Arc;

use async_trait::async_trait;
use happyview_provider::{
    BackfillError, BackfillKind, BackfillRequest, Host, ProviderError, RecordSource,
    db::{DatabaseBackend, adapt_sql},
};

use crate::admin::AdminError;
use crate::backfill::JobState;
use crate::consumer::{self, GapReplay, config::Settings};

/// Tables counted into telemetry `totals`, as (payload key, table name).
const COUNTED_TABLES: &[(&str, &str)] = &[
    ("backfill_repos", "happyview_backfill_repos"),
    ("backfill_queue", "happyview_backfill_queue"),
    ("backfill_completions", "happyview_backfill_completions"),
    ("backfill_pds_stats", "happyview_backfill_pds_stats"),
    ("backfill_cursors", "happyview_backfill_cursors"),
];

/// Days back a job still makes backfill an active feature in telemetry.
const ACTIVE_WINDOW_DAYS: i64 = 7;

pub struct JetstreamSource {
    settings: Settings,
    gap_replay: Option<GapReplay>,
    jobs: JobState,
}

impl JetstreamSource {
    /// A source consuming Jetstream by `settings`, replaying a refused
    /// cursor's gap by `gap_replay` (none: resume without the archive), with
    /// its backfill jobs in `jobs`.
    pub fn new(settings: Settings, gap_replay: Option<GapReplay>, jobs: JobState) -> Arc<Self> {
        Arc::new(Self {
            settings,
            gap_replay,
            jobs,
        })
    }

    /// The backfill job system this source runs.
    pub fn jobs(&self) -> &JobState {
        &self.jobs
    }
}

fn backfill_error(e: AdminError) -> BackfillError {
    match e {
        AdminError::BadRequest(_) => BackfillError::Invalid(e.to_string()),
        AdminError::Conflict(_) => BackfillError::Conflict(e.to_string()),
        _ => BackfillError::Internal(e.to_string()),
    }
}

#[async_trait]
impl RecordSource for JetstreamSource {
    fn id(&self) -> &'static str {
        "jetstream"
    }

    /// Its tables are still in HappyView's own migrations.
    fn migrations(&self, _backend: DatabaseBackend) -> Option<&'static sqlx::migrate::Migrator> {
        None
    }

    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        consumer::spawn_with_gap_replay(host, self.settings.clone(), None, self.gap_replay.clone());
        crate::backfill::resume_backfill_jobs(&self.jobs).await;
        let jobs = self.jobs.clone();
        tokio::spawn(async move {
            crate::backfill::run_backfill_retention_cleanup(&jobs).await;
        });
        Ok(())
    }

    fn admin_routes(&self) -> axum::Router {
        crate::admin::routes(self)
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        let jobs = &self.jobs;
        match req.kind {
            BackfillKind::Automatic => {
                let collection = req.collection.ok_or_else(|| {
                    BackfillError::Invalid("an automatic backfill needs a collection".into())
                })?;
                crate::backfill::start_backfill(jobs, Some(collection), Vec::new(), &req.actor)
                    .await
                    .map(Some)
                    .map_err(backfill_error)
            }
            BackfillKind::Manual => {
                let source = match req.source.as_deref() {
                    Some(raw) => crate::archive::parse_source(raw).ok_or_else(|| {
                        BackfillError::Invalid(format!("unknown backfill source: {raw}"))
                    })?,
                    None => jobs.default_source,
                };
                crate::backfill::start_backfill_with(
                    jobs,
                    req.collection,
                    req.dids,
                    &req.actor,
                    source,
                )
                .await
                .map(Some)
                .map_err(backfill_error)
            }
            BackfillKind::Gap { after_seq } => match &self.gap_replay {
                Some(replay) => replay(after_seq)
                    .await
                    .map(|(job_id, _tip)| Some(job_id))
                    .map_err(BackfillError::Internal),
                None => Err(BackfillError::Invalid(
                    "this source replays no gaps: backfill only crawls".into(),
                )),
            },
        }
    }

    /// Whether `collection` has ever had a network backfill job. A job for
    /// named accounts doesn't count: it fetched those accounts' records, not
    /// the collection's. When it cannot tell, it says yes, so no caller starts
    /// one on a guess.
    async fn has_backfill(&self, collection: &str) -> bool {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ? AND scope = 'network'",
            self.jobs.db_backend,
        );
        let seen: Result<(i64,), _> = happyview_provider::db::query_as(&sql)
            .bind(collection)
            .fetch_one(&self.jobs.db)
            .await;
        match seen {
            Ok((0,)) => false,
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    collection = %collection,
                    error = %e,
                    "could not tell whether this collection was backfilled; not starting one"
                );
                true
            }
        }
    }

    /// Whether a backfill covering `collection` may still write: running, or
    /// pausing or cancelling (both stop once in-flight pages are written).
    /// When it cannot tell, it says yes, so a delete still leaves its
    /// tombstone.
    async fn backfill_running(&self, collection: &str) -> bool {
        let sql = adapt_sql(
            "SELECT 1 FROM happyview_backfill_jobs \
             WHERE status IN ('running', 'pausing', 'cancelling') \
             AND (collection IS NULL OR collection = ?) LIMIT 1",
            self.jobs.db_backend,
        );
        match happyview_provider::db::query(&sql)
            .bind(collection)
            .fetch_optional(&self.jobs.db)
            .await
        {
            Ok(row) => row.is_some(),
            Err(e) => {
                tracing::warn!(
                    collection = %collection,
                    error = %e,
                    "could not tell whether a backfill is running; assuming one is"
                );
                true
            }
        }
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::to_value(consumer::status::snapshot()).unwrap_or_default()
    }

    /// The backfill tables' row counts (a table that cannot be counted is
    /// left out), and `feature.backfill.ever` / `feature.backfill.active`
    /// (any job, and any job in the last seven days).
    async fn metrics(&self) -> Vec<(String, i64)> {
        let mut out = Vec::new();
        for (key, table) in COUNTED_TABLES {
            if let Some(n) = self.count_table(table).await {
                out.push(((*key).to_string(), n));
            }
        }
        let ever = self
            .count_feature("SELECT COUNT(*) FROM happyview_backfill_jobs", None)
            .await
            >= 1;
        let since = (chrono::Utc::now() - chrono::Duration::days(ACTIVE_WINDOW_DAYS))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
        let active = ever
            && self
                .count_feature(
                    "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE created_at >= ?",
                    Some(&since),
                )
                .await
                >= 1;
        out.push(("feature.backfill.ever".to_string(), i64::from(ever)));
        out.push(("feature.backfill.active".to_string(), i64::from(active)));
        out
    }
}

impl JetstreamSource {
    async fn count_table(&self, table: &'static str) -> Option<i64> {
        let sql = adapt_sql(
            &format!("SELECT COUNT(*) FROM {table}"),
            self.jobs.db_backend,
        );
        match happyview_provider::db::query_as::<(i64,)>(&sql)
            .fetch_one(&self.jobs.db)
            .await
        {
            Ok((n,)) => Some(n),
            Err(e) => {
                tracing::debug!(table, error = %e, "telemetry could not count table");
                None
            }
        }
    }

    /// Run a count, treating any failure as zero.
    async fn count_feature(&self, sql: &str, bind: Option<&str>) -> i64 {
        let adapted = adapt_sql(sql, self.jobs.db_backend);
        let q = happyview_provider::db::query_as::<(i64,)>(&adapted);
        let q = match bind {
            Some(v) => q.bind(v),
            None => q,
        };
        match q.fetch_one(&self.jobs.db).await {
            Ok((n,)) => n,
            Err(e) => {
                tracing::warn!(error = %e, "telemetry feature probe failed, reporting unused");
                0
            }
        }
    }
}
