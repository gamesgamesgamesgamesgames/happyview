//! `JetstreamSource`: the Jetstream record source behind the `RecordSource`
//! contract: the live consumer, the backfill job system, archive replay and
//! their admin routes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use happyview_provider::{
    BackfillError, BackfillKind, BackfillRequest, Host, ProviderError, RecordSource,
    db::{DatabaseBackend, adapt_sql},
    telemetry,
};
use tokio::task::JoinHandle;

use crate::admin::AdminError;
use crate::archive::{ArchiveConfig, SourceChoice};
use crate::backfill::{JobState, RunningJobs};
use crate::consumer::{self, config::Settings, workers::RetryPolicy};

/// Tables counted into telemetry `totals`, as (payload key, table name).
const COUNTED_TABLES: &[(&str, &str)] = &[
    ("backfill_repos", "happyview_backfill_repos"),
    ("backfill_queue", "happyview_backfill_queue"),
    ("backfill_completions", "happyview_backfill_completions"),
    ("backfill_pds_stats", "happyview_backfill_pds_stats"),
    ("backfill_cursors", "happyview_backfill_cursors"),
];

/// The Jetstream a source built without settings consumes, when
/// `JETSTREAM_URL` is unset.
const DEFAULT_JETSTREAM_URL: &str = "wss://jetstream1.us-east.bsky.network";

pub struct JetstreamSource {
    /// `false` for a [`JetstreamSource::detached`] source: `start` spawns
    /// nothing.
    live: bool,
    /// `None`: `JETSTREAM_URL` and the other Jetstream settings, read when a
    /// host is given.
    settings: Option<Settings>,
    /// `None`: the replay settings, read when a host is given.
    archive: Option<ArchiveConfig>,
    /// Shared by every job system this source builds, so `backfill_running`
    /// sees each job it runs.
    running: RunningJobs,
    /// The job system over the host, once one is given.
    bound: RwLock<Option<Bound>>,
    /// The consumer and the retention sweep `start` spawned.
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// Set by the first `start`: a second one does nothing.
    started: AtomicBool,
}

#[derive(Clone)]
struct Bound {
    settings: Settings,
    jobs: JobState,
}

/// Builds a [`JetstreamSource`]. Settings not given are read from the
/// environment when the source is given its host.
#[derive(Default)]
pub struct JetstreamSourceBuilder {
    settings: Option<Settings>,
    archive: Option<ArchiveConfig>,
}

impl JetstreamSourceBuilder {
    /// Consume Jetstream by `settings` instead of the environment's.
    pub fn settings(mut self, settings: Settings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Replay the archive by `archive` instead of the environment's.
    pub fn archive(mut self, archive: ArchiveConfig) -> Self {
        self.archive = Some(archive);
        self
    }

    /// The source a served instance runs.
    pub fn build(self) -> Arc<JetstreamSource> {
        Arc::new(self.source(true))
    }

    /// A source whose `start` spawns nothing; [`JetstreamSource::attach`]
    /// gives it a host.
    pub fn build_detached(self) -> Arc<JetstreamSource> {
        Arc::new(self.source(false))
    }

    fn source(self, live: bool) -> JetstreamSource {
        JetstreamSource {
            live,
            settings: self.settings,
            archive: self.archive,
            running: RunningJobs::default(),
            bound: RwLock::new(None),
            tasks: Mutex::new(Vec::new()),
            started: AtomicBool::new(false),
        }
    }
}

impl JetstreamSource {
    pub fn builder() -> JetstreamSourceBuilder {
        JetstreamSourceBuilder::default()
    }

    /// A source whose `start` spawns nothing and that has no host until
    /// [`Self::attach`] gives it one.
    pub fn detached() -> Arc<Self> {
        Self::builder().build_detached()
    }

    /// Give this source `host` without starting anything: its backfill jobs,
    /// admin routes and answers work over `host` from now on. `start` does
    /// this itself.
    pub fn attach(&self, host: Arc<dyn Host>) {
        let settings = self.settings.clone().unwrap_or_else(|| {
            Settings::from_env(
                &happyview_provider::vars::var("JETSTREAM_URL")
                    .unwrap_or_else(|| DEFAULT_JETSTREAM_URL.to_string()),
            )
        });
        let archive = self
            .archive
            .clone()
            .unwrap_or_else(|| ArchiveConfig::from_env(&host.instance().user_agent));
        let mut jobs = JobState::new(host, archive, settings.endpoint.clone());
        jobs.running = self.running.clone();
        *self.bound.write().unwrap_or_else(|e| e.into_inner()) = Some(Bound { settings, jobs });
    }

    fn bound(&self) -> Option<Bound> {
        self.bound.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The jobs this source runs, as `backfill_running` answers from them.
    pub fn running(&self) -> RunningJobs {
        self.running.clone()
    }

    /// The backfill job system this source runs, once it has a host.
    pub fn jobs(&self) -> Option<JobState> {
        self.bound().map(|bound| bound.jobs)
    }

    /// Start the live consumer over `host` by `settings`, retrying failed
    /// events by `retry`. A cursor the server refuses as too old is replayed
    /// from the archive as one of this source's jobs, unless backfill only
    /// crawls (P8), which never asks the archive. Gives the source `host`
    /// first if it has none.
    pub fn spawn_consumer(
        &self,
        host: Arc<dyn Host>,
        settings: Settings,
        retry: Option<RetryPolicy>,
    ) -> JoinHandle<()> {
        let jobs = match self.jobs() {
            Some(jobs) => jobs.for_host(host.clone()),
            None => {
                self.attach(host.clone());
                self.jobs().expect("attached above")
            }
        };
        let gap_jobs = (jobs.archive.default_source != SourceChoice::Crawl).then_some(jobs);
        consumer::spawn_replaying(host, settings, retry, gap_jobs)
    }

    /// Stop what `start` spawned: the live consumer and the retention
    /// sweep. Running backfill jobs are left to finish. A started source is
    /// usually held for the life of the process (its host's state holds it
    /// back), so this is reached by an explicit call; dropping a source that
    /// is no longer held calls it too.
    pub fn stop(&self) {
        for task in self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

impl Drop for JetstreamSource {
    fn drop(&mut self) {
        self.stop();
    }
}

fn backfill_error(e: AdminError) -> BackfillError {
    match e {
        AdminError::BadRequest(_) => BackfillError::Invalid(e.to_string()),
        AdminError::Conflict(_) => BackfillError::Conflict(e.to_string()),
        _ => BackfillError::Internal(e.to_string()),
    }
}

fn unbound_error() -> BackfillError {
    BackfillError::Internal("the jetstream source has no host yet".into())
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

    /// Resume the jobs the last run left running, then spawn the consumer
    /// and the retention sweep. Once: a second call does nothing.
    async fn start(self: Arc<Self>, host: Arc<dyn Host>) -> Result<(), ProviderError> {
        // Once only: a second start would resume, and so run twice, the
        // jobs the first one is running.
        if !self.live || self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.attach(host.clone());
        let Bound { settings, jobs } = self.bound().expect("attached above");
        // Before the consumer, so a job the last run left running is known
        // to be running before a live delete asks.
        crate::backfill::resume_backfill_jobs(&jobs).await;
        let consumer = self.spawn_consumer(host, settings, None);
        let retention = {
            let jobs = jobs.clone();
            tokio::spawn(async move {
                crate::backfill::run_backfill_retention_cleanup(&jobs).await;
            })
        };
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend([consumer, retention]);
        Ok(())
    }

    fn admin_routes(&self) -> axum::Router {
        match self.jobs() {
            Some(jobs) => crate::admin::routes(jobs),
            None => axum::Router::new(),
        }
    }

    async fn start_backfill(&self, req: BackfillRequest) -> Result<Option<String>, BackfillError> {
        let jobs = &self.jobs().ok_or_else(unbound_error)?;
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
                    None => jobs.archive.default_source,
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
            // Asked for, a gap is replayed whatever `BACKFILL_SOURCE` says;
            // only the consumer's own gap replay is off under `crawl`.
            BackfillKind::Gap { after_seq } => {
                crate::backfill::replay::start_gap_replay(jobs, after_seq)
                    .await
                    .map(|(job_id, _tip)| Some(job_id))
                    .map_err(BackfillError::Internal)
            }
        }
    }

    /// Whether `collection` has ever had a network backfill job. A job for
    /// named accounts doesn't count: it fetched those accounts' records, not
    /// the collection's. When it cannot tell, it says yes, so no caller starts
    /// one on a guess.
    async fn has_backfill(&self, collection: &str) -> bool {
        let Some(jobs) = self.jobs() else {
            return false;
        };
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_backfill_jobs WHERE collection = ? AND scope = 'network'",
            jobs.db_backend,
        );
        let seen: Result<(i64,), _> = happyview_provider::db::query_as(&sql)
            .bind(collection)
            .fetch_one(&jobs.db)
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

    /// Whether a backfill covering `collection` may still write: a job of
    /// this source's running, pausing or cancelling (both stop once in-flight
    /// pages are written). Answered from the jobs this source runs, without
    /// a query.
    async fn backfill_running(&self, collection: &str) -> bool {
        self.running.covers(collection)
    }

    async fn status(&self) -> serde_json::Value {
        serde_json::to_value(consumer::status::snapshot()).unwrap_or_default()
    }

    /// The backfill tables' row counts (a table that cannot be counted is
    /// left out), and `feature.backfill.ever` / `feature.backfill.active`
    /// (any job, and any job in the last seven days).
    async fn metrics(&self) -> Vec<(String, i64)> {
        let Some(jobs) = self.jobs() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (key, table) in COUNTED_TABLES {
            if let Some(n) = telemetry::count_table(&jobs.db, jobs.db_backend, table).await {
                out.push(((*key).to_string(), n));
            }
        }
        let (db, backend) = (&jobs.db, jobs.db_backend);
        let ever = telemetry::count(
            db,
            backend,
            "SELECT COUNT(*) FROM happyview_backfill_jobs",
            None,
        )
        .await
            >= 1;
        let since = telemetry::window_start();
        let active = ever
            && telemetry::count(
                db,
                backend,
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
