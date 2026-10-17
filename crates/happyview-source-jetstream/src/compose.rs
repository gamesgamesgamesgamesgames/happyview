//! Composing the Jetstream source into another provider: which tracked
//! collections it streams live ([`crate::JetstreamSourceBuilder::live_filter`])
//! and who runs a collection's backfill
//! ([`crate::JetstreamSourceBuilder::executor_for`]).
//!
//! A [`BackfillExecutor`]'s job is an ordinary row of the job system: it is
//! listed, paused, cancelled, resumed and swept like a crawl, its `source`
//! column holds the executor's [`BackfillExecutor::name`], and its progress
//! reaches the job event stream through its [`JobHandle`].

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use happyview_provider::Host;
use happyview_provider::db::adapt_sql;
use happyview_provider::failure::{BackfillErrorKind, BackfillFailure};
use tokio::sync::watch;

use crate::backfill::errors::ErrorRecorder;
use crate::backfill::{self, JobState, types::BackfillEvent};

/// Runs the backfills of the collections it is given for, in place of a
/// crawl or an archive replay.
#[async_trait::async_trait]
pub trait BackfillExecutor: Send + Sync + 'static {
    /// A short name stored as the job's `source` (e.g. "store-pass"). Never
    /// `crawl`, `replay` or `auto`: those name the source's own backfills.
    fn name(&self) -> &'static str;
    /// Backfill `collection`. Return early once [`JobHandle::should_stop`]
    /// says so; the job is then paused or cancelled, whatever this returns.
    async fn run(&self, job: JobHandle, collection: &str) -> Result<JobTotals, String>;
}

/// What an executor's run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobTotals {
    /// Records the run wrote; the job's `total_records`.
    pub records: i64,
}

/// An executor's handle on the job it runs.
#[derive(Clone)]
pub struct JobHandle {
    jobs: JobState,
    id: Arc<str>,
    collection: Arc<str>,
    /// The last `processed` reported, which completing the job keeps.
    processed: Arc<AtomicI64>,
    errors: Arc<ErrorRecorder>,
}

impl JobHandle {
    pub(crate) async fn new(jobs: JobState, id: &str, collection: &str) -> Self {
        let processed = processed_so_far(&jobs, id).await;
        let errors = ErrorRecorder::new(&jobs, id).await;
        Self {
            jobs,
            id: id.into(),
            collection: collection.into(),
            processed: Arc::new(AtomicI64::new(processed)),
            errors: Arc::new(errors),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn host(&self) -> &Arc<dyn Host> {
        &self.jobs.host
    }

    /// `processed` of `total` (when known) done: the job's `processed_repos`
    /// and `total_repos`, sent to the job's event stream.
    pub async fn progress(&self, processed: i64, total: Option<i64>) {
        self.processed.store(processed, Ordering::Relaxed);
        let processed = clamp(processed);
        let total = total.map(clamp);
        backfill::update_job_counter(&self.jobs, &self.id, "processed_repos", processed).await;
        if let Some(total) = total {
            backfill::update_job_counter(&self.jobs, &self.id, "total_repos", total).await;
        }
        backfill::publish_event(
            &self.jobs,
            BackfillEvent::JobCounters {
                job_id: self.id.to_string(),
                total_repos: total,
                resolved_repos: None,
                processed_repos: Some(processed),
                total_records: None,
            },
        );
    }

    /// The job's stage, shown as it is.
    pub async fn stage(&self, stage: &str) {
        backfill::set_stage(&self.jobs, &self.id, stage).await;
    }

    /// Note a failure in the job's error log (kind `other`, phase
    /// `executor`), against `did` or, with `None`, the job as a whole. The
    /// log keeps one row per DID: a later failure replaces its message.
    pub async fn record_error(&self, did: Option<&str>, message: &str) {
        let failure = BackfillFailure {
            kind: BackfillErrorKind::Other,
            message: message.to_string(),
            retry_after: None,
        };
        self.errors
            .record(
                &self.jobs,
                &self.id,
                did.unwrap_or(""),
                Some(&self.collection),
                EXECUTOR_PHASE,
                &failure,
                1,
            )
            .await;
    }

    /// True once the job is cancelled or pausing; the executor should stop.
    pub async fn should_stop(&self) -> bool {
        backfill::should_stop_worker(&self.jobs, &self.id).await
    }

    /// Executor-owned resume point, persisted in the job row: the job's
    /// collection's cursor (`happyview_backfill_cursors.relay_cursor`), which
    /// only a crawl's discovery otherwise writes.
    pub async fn checkpoint(&self, cursor: &str) {
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES (?, ?, ?, 0) \
             ON CONFLICT (job_id, collection) DO UPDATE SET relay_cursor = excluded.relay_cursor",
            self.jobs.db_backend,
        );
        backfill::job_write(&self.jobs, &self.id, "checkpoint", || {
            happyview_provider::db::query(&sql)
                .bind(&*self.id)
                .bind(&*self.collection)
                .bind(cursor)
                .execute(&self.jobs.backfill_db)
        })
        .await;
    }

    /// The last [`Self::checkpoint`] of this job, across pauses and restarts.
    pub async fn resume_from(&self) -> Option<String> {
        let sql = adapt_sql(
            "SELECT relay_cursor FROM happyview_backfill_cursors WHERE job_id = ? AND collection = ?",
            self.jobs.db_backend,
        );
        happyview_provider::db::query_as::<(Option<String>,)>(&sql)
            .bind(&*self.id)
            .bind(&*self.collection)
            .fetch_optional(&self.jobs.backfill_db)
            .await
            .ok()
            .flatten()
            .and_then(|(cursor,)| cursor)
    }

    pub(crate) fn processed(&self) -> i32 {
        clamp(self.processed.load(Ordering::Relaxed))
    }

    pub(crate) async fn flush_errors(&self) {
        self.errors.flush(&self.jobs, &self.id).await;
    }
}

/// The phase an executor's failures are logged under.
pub const EXECUTOR_PHASE: &str = "executor";

/// The job counters are `INT` columns.
fn clamp(n: i64) -> i32 {
    i32::try_from(n.max(0)).unwrap_or(i32::MAX)
}

async fn processed_so_far(jobs: &JobState, id: &str) -> i64 {
    let sql = adapt_sql(
        "SELECT processed_repos FROM happyview_backfill_jobs WHERE id = ?",
        jobs.db_backend,
    );
    happyview_provider::db::query_as::<(Option<i32>,)>(&sql)
        .bind(id)
        .fetch_optional(&jobs.backfill_db)
        .await
        .ok()
        .flatten()
        .and_then(|(n,)| n)
        .map(i64::from)
        .unwrap_or(0)
}

/// Which tracked collections are streamed live.
pub type LiveFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;
/// Who runs a collection's backfill; `None`: crawl or replay.
pub type ExecutorFor = Arc<dyn Fn(&str) -> Option<Arc<dyn BackfillExecutor>> + Send + Sync>;

/// How a composing provider shapes the source. The default streams every
/// tracked collection and crawls or replays every backfill.
#[derive(Clone, Default)]
pub struct Composition {
    pub live_filter: Option<LiveFilter>,
    pub executor_for: Option<ExecutorFor>,
}

impl Composition {
    /// Whether `collection` is streamed live (when tracked).
    pub fn streams(&self, collection: &str) -> bool {
        self.live_filter.as_ref().is_none_or(|f| f(collection))
    }

    /// The `tracked` collections streamed live.
    pub fn live(&self, tracked: &[String]) -> Vec<String> {
        tracked
            .iter()
            .filter(|c| self.streams(c))
            .cloned()
            .collect()
    }

    /// The executor that backfills `collection`, if it isn't crawled or
    /// replayed.
    pub fn executor(&self, collection: &str) -> Option<Arc<dyn BackfillExecutor>> {
        self.executor_for.as_ref().and_then(|f| f(collection))
    }

    /// The tracked collections streamed live, kept current: it changes when
    /// the tracked list changes what is streamed. Without a filter, the
    /// tracked list itself.
    pub fn live_collections(
        &self,
        mut tracked: watch::Receiver<Vec<String>>,
    ) -> watch::Receiver<Vec<String>> {
        if self.live_filter.is_none() {
            return tracked;
        }
        let compose = self.clone();
        let (tx, rx) = watch::channel(compose.live(&tracked.borrow_and_update()));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = tracked.changed() => {
                        // The tracked list can't change again; dropping `tx`
                        // tells the consumer so.
                        if changed.is_err() {
                            break;
                        }
                    }
                    // Nobody streams from it any more.
                    _ = tx.closed() => break,
                }
                let live = compose.live(&tracked.borrow_and_update());
                tx.send_if_modified(|current| {
                    let modified = *current != live;
                    if modified {
                        *current = live;
                    }
                    modified
                });
            }
        });
        rx
    }
}
