//! The backfill job system: job rows, the run loop, resume, retention and
//! the job event channel. Crawl is in [`crawl`], archive replay in
//! [`replay`].
//!
//! Items are `pub` for HappyView's database-backed tests, which need a host
//! this crate cannot build; they are not an API.

pub mod crawl;
pub mod errors;
pub mod replay;
pub mod replay_fold;
pub mod replay_sweeps;
pub mod retry;
pub mod types;

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use rand::RngExt;
use tokio::sync::broadcast;
use uuid::Uuid;

pub use happyview_provider::db::now_rfc3339;
use happyview_provider::db::{DatabaseBackend, adapt_sql};
use happyview_provider::{Host, ProviderEvent, Severity};

use crate::admin::AdminError;
use crate::archive::{ArchiveConfig, SourceChoice};
use crate::compose::{BackfillExecutor, Composition, JobHandle};
use crate::consumer::config::Endpoint;

pub use crawl::*;
use types::BackfillEvent;

use crate::admin::DIDS_GONE;

/// Jobs the event channel holds before a slow SSE client lags and resyncs
/// from a snapshot.
const EVENT_CHANNEL_CAPACITY: usize = 16384;

/// What the job system works with: the host, its pools, the archive replay
/// settings and the job event channel. Cheap to clone: every field is a
/// handle.
#[derive(Clone)]
pub struct JobState {
    pub host: Arc<dyn Host>,
    pub db: sqlx::AnyPool,
    /// The pool sized for backfill work (`BACKFILL_CONCURRENT_*`).
    pub backfill_db: sqlx::AnyPool,
    pub db_backend: DatabaseBackend,
    pub http: reqwest::Client,
    pub config: JobConfig,
    /// Replay settings (`JETSTREAM_API_KEY`, `BACKFILL_SOURCE`, ...).
    /// `archive.default_source` is the source a job uses when its request
    /// names none.
    pub archive: ArchiveConfig,
    /// The Jetstream whose archive replay jobs read.
    pub endpoint: Endpoint,
    pub backfill_events_tx: broadcast::Sender<BackfillEvent>,
    /// The jobs running in this process.
    pub running: RunningJobs,
    /// What a composing provider streams live and backfills itself.
    pub compose: Composition,
}

/// The addresses a crawl asks.
#[derive(Clone, Debug)]
pub struct JobConfig {
    pub plc_url: String,
    pub relay_url: String,
}

impl JobState {
    pub fn new(host: Arc<dyn Host>, archive: ArchiveConfig, endpoint: Endpoint) -> Self {
        let db = host.database();
        let instance = host.instance();
        Self {
            db: db.pool,
            backfill_db: db.backfill_pool,
            db_backend: db.backend,
            http: host.http(),
            config: JobConfig {
                plc_url: instance.plc_url,
                relay_url: instance.relay_url,
            },
            archive,
            endpoint,
            backfill_events_tx: broadcast::channel(EVENT_CHANNEL_CAPACITY).0,
            running: RunningJobs::default(),
            compose: Composition::default(),
            host,
        }
    }

    /// These jobs over `host` instead: its pools and addresses, with this
    /// state's settings, event channel and running jobs.
    pub fn for_host(&self, host: Arc<dyn Host>) -> Self {
        Self {
            backfill_events_tx: self.backfill_events_tx.clone(),
            running: self.running.clone(),
            compose: self.compose.clone(),
            ..Self::new(host, self.archive.clone(), self.endpoint.clone())
        }
    }
}

/// The backfill jobs whose run is in progress in this process, with the
/// collection each covers (`None`: every collection). A job is in it from
/// the moment [`spawn_job`] spawns its run (or the run loads it, for a run
/// started any other way) until the run returns, which covers the
/// `running`, `pausing` and `cancelling` statuses: a pausing or cancelling
/// job still writes the pages it has in flight.
#[derive(Clone, Default)]
pub struct RunningJobs(Arc<Mutex<RunningMap>>);

/// Each running job's collection and how many guards hold it.
type RunningMap = HashMap<String, (Option<String>, usize)>;

impl RunningJobs {
    /// Note `job_id` as running until the guard is dropped. Guards nest: the
    /// job runs until the last one goes.
    pub fn enter(&self, job_id: &str, collection: Option<String>) -> RunningGuard {
        self.lock()
            .entry(job_id.to_string())
            .or_insert((collection, 0))
            .1 += 1;
        RunningGuard {
            jobs: self.clone(),
            job_id: job_id.to_string(),
        }
    }

    /// Whether a running job covers `collection`.
    pub fn covers(&self, collection: &str) -> bool {
        self.lock()
            .values()
            .any(|(covered, _)| covered.as_deref().is_none_or(|c| c == collection))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RunningMap> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Removes its job from [`RunningJobs`] when dropped.
pub struct RunningGuard {
    jobs: RunningJobs,
    job_id: String,
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        let mut jobs = self.jobs.lock();
        if let Some((_, guards)) = jobs.get_mut(&self.job_id) {
            *guards -= 1;
            if *guards == 0 {
                jobs.remove(&self.job_id);
            }
        }
    }
}

/// How a replay job's run ended.
pub(crate) enum ReplayOutcome {
    Completed {
        records: u64,
        bytes: u64,
    },
    /// `auto`, and the Jetstream has no archive: the job is now a crawl.
    FellBack,
    /// Pausing or cancelling; the caller finalises.
    Stopped,
    Failed(String),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) async fn set_stage(state: &JobState, job_id: &str, stage: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET stage = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "stage", || {
        happyview_provider::db::query(&sql)
            .bind(stage)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobStageChanged {
            job_id: job_id.to_string(),
            stage: stage.to_string(),
        },
    );
}

pub(crate) async fn set_note(state: &JobState, job_id: &str, note: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET note = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "note", || {
        happyview_provider::db::query(&sql)
            .bind(note)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobNote {
            job_id: job_id.to_string(),
            note: note.to_string(),
        },
    );
}

pub(crate) async fn update_job_counter(state: &JobState, job_id: &str, column: &str, value: i32) {
    let query = match column {
        "total_repos" => "UPDATE happyview_backfill_jobs SET total_repos = ? WHERE id = ?",
        "resolved_repos" => "UPDATE happyview_backfill_jobs SET resolved_repos = ? WHERE id = ?",
        "processed_repos" => "UPDATE happyview_backfill_jobs SET processed_repos = ? WHERE id = ?",
        "total_records" => "UPDATE happyview_backfill_jobs SET total_records = ? WHERE id = ?",
        other => {
            tracing::error!(
                column = other,
                "update_job_counter called with unknown column"
            );
            return;
        }
    };
    let sql = adapt_sql(query, state.db_backend);
    job_write(state, job_id, "counter", || {
        happyview_provider::db::query(&sql)
            .bind(value)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

/// `column + increment` for a job counter, held at `i32::MAX`: the counter
/// columns are `INT`, which Postgres refuses to overflow and which every
/// reader decodes as `i32`. Postgres widens the sum first so that it cannot
/// overflow before it is clamped.
pub(crate) fn saturating_counter(
    column: &str,
    increment: &str,
    backend: happyview_provider::db::DatabaseBackend,
) -> String {
    match backend {
        happyview_provider::db::DatabaseBackend::Sqlite => {
            format!("{column} = MIN(COALESCE({column}, 0) + {increment}, 2147483647)")
        }
        happyview_provider::db::DatabaseBackend::Postgres => format!(
            "{column} = LEAST(CAST(COALESCE({column}, 0) AS BIGINT) + {increment}, 2147483647)"
        ),
    }
}

/// Add `n` to an in-memory job counter, holding it at `i32::MAX` like its
/// column. Returns the new value.
pub fn saturating_fetch_add(counter: &AtomicI32, n: i32) -> i32 {
    let previous = counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            Some(v.saturating_add(n))
        })
        .unwrap_or_else(|v| v);
    previous.saturating_add(n)
}

pub(crate) fn publish_event(state: &JobState, event: types::BackfillEvent) {
    let _ = state.backfill_events_tx.send(event);
}

/// How often a running job asks its host whether new hook stats are due. The
/// host decides how often they are, so this only bounds how late they go out.
const HOOK_STATS_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Publishes a running job's hook stats as its host makes them due, until
/// dropped.
struct HookStatsPublisher(tokio::task::JoinHandle<()>);

impl HookStatsPublisher {
    fn spawn(state: &JobState, job_id: &str) -> Self {
        let state = state.clone();
        let job_id = job_id.to_string();
        Self(tokio::spawn(async move {
            loop {
                tokio::time::sleep(HOOK_STATS_POLL).await;
                if let Some(stats) = state.host.job_hook_stats_due(&job_id) {
                    publish_event(
                        &state,
                        types::BackfillEvent::HookStats {
                            job_id: job_id.clone(),
                            stats,
                        },
                    );
                }
            }
        }))
    }
}

impl Drop for HookStatsPublisher {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Keep a job's final hook stats with it as it ends, and publish them ahead
/// of its completion. A job none of whose records reached a hook keeps none.
async fn keep_hook_stats(state: &JobState, job_id: &str) {
    let Some(stats) = state.host.finish_job_hook_stats(job_id) else {
        return;
    };
    let json = match serde_json::to_string(&stats) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(job_id, error = %e, "could not encode a job's hook stats");
            return;
        }
    };
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET hook_stats = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "hook_stats", || {
        happyview_provider::db::query(&sql)
            .bind(&json)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::HookStats {
            job_id: job_id.to_string(),
            stats,
        },
    );
}

/// Run one bookkeeping write for a job, retrying while the database is busy.
/// A write that still fails is logged and recorded in the event log rather
/// than dropped, so a counter or stage that stopped moving can be traced.
pub async fn job_write<T, F, Fut>(
    state: &JobState,
    job_id: &str,
    what: &'static str,
    op: F,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    match happyview_provider::db::retry_on_busy(op).await {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::error!(job_id, what, error = %e, "backfill bookkeeping write failed");
            state
                .host
                .log_event(ProviderEvent {
                    event_type: "backfill.write_failed".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(job_id.to_string()),
                    detail: serde_json::json!({
                        "job_id": job_id,
                        "write": what,
                        "error": e.to_string(),
                    }),
                })
                .await;
            None
        }
    }
}

/// Current job state, straight from the database.
///
/// The SSE stream is otherwise delta-only over a lossy broadcast channel, so a
/// client that connects mid-phase or misses an event has no way to recover.
/// A snapshot is how it resyncs.
pub async fn build_job_snapshot(state: &JobState, job_id: &str) -> Option<types::BackfillEvent> {
    let sql = adapt_sql(
        "SELECT status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts, discovery_complete \
         FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    #[allow(clippy::type_complexity)]
    let row: Option<(
        String,
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        i32,
    )> = happyview_provider::db::query_as(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let (
        status,
        stage,
        total_repos,
        resolved_repos,
        processed_repos,
        total_records,
        error_counts,
        discovery_complete,
    ) = row?;

    Some(types::BackfillEvent::JobSnapshot {
        job_id: job_id.to_string(),
        status,
        stage,
        total_repos,
        resolved_repos,
        processed_repos,
        total_records,
        error_counts: error_counts
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({})),
        discovery_complete: discovery_complete != 0,
    })
}

pub(crate) fn random_batch_threshold(base: i32) -> i32 {
    let low = base - base / 10;
    rand::rng().random_range(low..=base)
}

pub(crate) struct BackfillConcurrency {
    resolution: usize,
    pds: usize,
    dids_per_pds: usize,
}

pub(crate) async fn load_concurrency(state: &JobState) -> BackfillConcurrency {
    let resolution = state
        .host
        .setting("backfill_concurrent_resolution")
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(100usize)
        .max(1);
    let pds = state
        .host
        .setting("backfill_concurrent_pds")
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(10usize)
        .max(1);
    let dids_per_pds = state
        .host
        .setting("backfill_concurrent_dids_per_pds")
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(3usize)
        .max(1);
    BackfillConcurrency {
        resolution,
        pds,
        dids_per_pds,
    }
}

pub(crate) async fn load_max_attempts(state: &JobState) -> u32 {
    state
        .host
        .setting("backfill_max_attempts")
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(3u32)
        .clamp(1, 10)
}

pub(crate) async fn fail_job(state: &JobState, job_id: &str, error: &str) {
    keep_hook_stats(state, job_id).await;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'failed', completed_at = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "fail", || {
        happyview_provider::db::query(&sql)
            .bind(&now)
            .bind(error)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "failed".to_string(),
            error: Some(error.to_string()),
        },
    );
}

pub(crate) async fn should_stop(state: &JobState, job_id: &str) -> Option<&'static str> {
    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let status = happyview_provider::db::query_as::<(String,)>(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten()
        .map(|(s,)| s);
    match status.as_deref() {
        Some("cancelling") => Some("cancelling"),
        Some("pausing") => Some("pausing"),
        _ => None,
    }
}

pub(crate) async fn should_stop_worker(state: &JobState, job_id: &str) -> bool {
    should_stop(state, job_id).await.is_some()
}

pub(crate) async fn request_cancel(state: &JobState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelling' WHERE id = ? AND status IN ('running', 'paused')",
        state.db_backend,
    );
    job_write(state, job_id, "request_cancel", || {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

pub(crate) async fn finalise_cancel(state: &JobState, job_id: &str) {
    keep_hook_stats(state, job_id).await;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelled', completed_at = ?, error = 'cancelled by user' WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "cancel", || {
        happyview_provider::db::query(&sql)
            .bind(&now)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "cancelled".to_string(),
            error: None,
        },
    );
}

pub async fn request_pause(state: &JobState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'pausing' WHERE id = ? AND status = 'running'",
        state.db_backend,
    );
    job_write(state, job_id, "request_pause", || {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

pub(crate) async fn finalise_pause(state: &JobState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'paused' WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "pause", || {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "paused".to_string(),
            error: None,
        },
    );
}

pub(crate) async fn complete_job(
    state: &JobState,
    job_id: &str,
    processed_repos: i32,
    total_records: i32,
    error: Option<&str>,
) {
    keep_hook_stats(state, job_id).await;
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'completed', stage = 'completed', completed_at = ?, processed_repos = ?, total_records = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "complete", || {
        happyview_provider::db::query(&sql)
            .bind(&now)
            .bind(processed_repos)
            .bind(total_records)
            .bind(error)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "completed".to_string(),
            error: error.map(|e| e.to_string()),
        },
    );
}

// ---------------------------------------------------------------------------
// Background backfill worker
// ---------------------------------------------------------------------------

/// Spawn `job_id`'s run, which covers `collection`, noted as running from
/// now rather than from when its task first runs: whoever started the job
/// sees it running as soon as the start returns. The run notes itself as
/// well, and the two guards nest.
pub(crate) fn spawn_job(state: &JobState, job_id: &str, collection: Option<String>) {
    let running = state.running.enter(job_id, collection);
    let spawn_state = state.clone();
    let spawn_job_id = job_id.to_string();
    tokio::spawn(async move {
        run_backfill_job(spawn_state, spawn_job_id).await;
        drop(running);
    });
}

pub(crate) async fn run_backfill_job(state: JobState, job_id: String) {
    let window = parse_discovery_window(
        happyview_provider::vars::var("BACKFILL_DISCOVERY_WINDOW").as_deref(),
    );
    run_backfill_job_with(state, job_id, window).await;
}

pub async fn run_backfill_job_with(state: JobState, job_id: String, window: i64) {
    let backend = state.db_backend;

    // Load job metadata
    let sql = adapt_sql(
        "SELECT collection, did, stage, queue_version, discovery_complete, scope, source, replay_after_seq FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let job: Option<(
        Option<String>,
        Option<String>,
        String,
        i32,
        i32,
        String,
        String,
        Option<i64>,
    )> = happyview_provider::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let Some((
        collection,
        did,
        stage,
        queue_version,
        discovery_complete,
        scope,
        source,
        replay_after_seq,
    )) = job
    else {
        tracing::error!(job_id, "backfill job not found");
        return;
    };
    let _running = state.running.enter(&job_id, collection.clone());
    let _hook_stats = HookStatsPublisher::spawn(&state, &job_id);

    // Determine target collections. A collection with an executor belongs
    // to the provider that composes this source: a crawl or replay of every
    // collection, and any account backfill, leaves it to that executor.
    let mut left_out: Vec<String> = Vec::new();
    let collections: Vec<String> = if let Some(ref col) = collection {
        let lexicon_exists: bool = state.host.tracked_collections().borrow().contains(col);
        if !lexicon_exists {
            let error = format!("no indexed record lexicon for collection '{col}'");
            fail_job(&state, &job_id, &error).await;
            return;
        }
        if scope == "dids" && state.compose.executor(col).is_some() {
            left_out.push(col.clone());
            Vec::new()
        } else {
            vec![col.clone()]
        }
    } else {
        let tracked = state.host.tracked_collections().borrow().clone();
        if replay_after_seq.is_some() {
            // A gap is in the live stream: replay what it streams, as
            // `start_gap_replay` planned, executor or not.
            tracked
                .into_iter()
                .filter(|c| state.compose.streams(c))
                .collect()
        } else {
            let (owned, rest): (Vec<String>, Vec<String>) = tracked
                .into_iter()
                .partition(|c| state.compose.executor(c).is_some());
            left_out = owned;
            rest
        }
    };
    if !left_out.is_empty() {
        tracing::info!(
            job_id,
            collections = ?left_out,
            "leaving collections with a backfill executor out of this backfill"
        );
    }

    if collections.is_empty() {
        complete_job(
            &state,
            &job_id,
            0,
            0,
            Some("no backfill-eligible collections"),
        )
        .await;
        return;
    }

    // Not one of the source's own: an executor's job.
    if !crate::archive::is_builtin_source(&source) {
        run_executor_job(&state, &job_id, collection.as_deref(), &source).await;
        return;
    }

    if source != "crawl" {
        let scope_dids = if scope == "dids" {
            match replay_sweeps::job_dids(&state, &job_id).await {
                // Never plan an account job without its accounts.
                Ok(dids) if dids.is_empty() => {
                    fail_job(&state, &job_id, DIDS_GONE).await;
                    return;
                }
                Ok(dids) => Some(dids),
                Err(e) => {
                    fail_job(
                        &state,
                        &job_id,
                        &format!("failed to read the job's accounts: {e}"),
                    )
                    .await;
                    return;
                }
            }
        } else {
            None
        };
        match replay::run(&state, &job_id, &collections, scope_dids, &source).await {
            ReplayOutcome::FellBack => {}
            ReplayOutcome::Completed { records, bytes } => {
                let records = i32::try_from(records).unwrap_or(i32::MAX);
                complete_job(&state, &job_id, 0, records, None).await;
                state
                    .host
                    .log_event(ProviderEvent {
                        event_type: "backfill.completed".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: collection,
                        detail: serde_json::json!({
                            "job_id": job_id,
                            "source": "replay",
                            "total_records": records,
                            "bytes_downloaded": bytes,
                        }),
                    })
                    .await;
                return;
            }
            ReplayOutcome::Stopped => {
                match should_stop(&state, &job_id).await {
                    Some("cancelling") => finalise_cancel(&state, &job_id).await,
                    _ => finalise_pause(&state, &job_id).await,
                }
                return;
            }
            ReplayOutcome::Failed(error) => {
                tracing::error!(job_id, error, "replay backfill failed");
                fail_job(&state, &job_id, &error).await;
                return;
            }
        }
    }

    let mut version = QueueVersion::from_column(queue_version);
    let mut discovery_complete = discovery_complete != 0;
    // A pre-upgrade job that had not finished discovering starts discovery
    // again on the bounded queue. One already past discovery finishes on the
    // rows it has. A job with a `did` targets that one account (see the
    // `backfill_job_scope` migration).
    if version == QueueVersion::Legacy && matches!(stage.as_str(), "pending" | "discovering_repos")
    {
        if let Err(e) = convert_to_bounded(&state, &job_id, did.as_deref()).await {
            let error = format!("failed to move the job onto the bounded queue: {e}");
            fail_job(&state, &job_id, &error).await;
            return;
        }
        version = QueueVersion::Bounded;
        discovery_complete = did.is_some();
    }

    let concurrency = load_concurrency(&state).await;
    let outcome = if version == QueueVersion::Legacy && stage == "fetching_records" {
        // Resolution finished under a version that predates the pipeline.
        Ok(run_fetching_phase(&state, &job_id, &collections, &concurrency).await)
    } else {
        run_queue(
            &state,
            &job_id,
            &collections,
            &concurrency,
            version,
            discovery_complete,
            window,
        )
        .await
    };
    let (final_processed, final_records) = match outcome {
        Ok(counts) => counts,
        Err(error) => {
            tracing::error!(job_id, error, "backfill job failed");
            fail_job(&state, &job_id, &error).await;
            return;
        }
    };

    match should_stop(&state, &job_id).await {
        Some("cancelling") => {
            tracing::info!(job_id, "backfill job cancelled");
            finalise_cancel(&state, &job_id).await;
            return;
        }
        Some("pausing") => {
            tracing::info!(job_id, "backfill job paused");
            finalise_pause(&state, &job_id).await;
            return;
        }
        _ => {}
    }

    // A unit can stay queued when a write about it failed (its PDS was never
    // stored, or its completion was never committed). The job is not done:
    // leave it paused so resuming picks those units up again.
    if version == QueueVersion::Bounded {
        match queued_units(&state, &job_id).await {
            Ok(0) => {}
            Ok(left) => {
                pause_with_units_left(&state, &job_id, collection.as_deref(), left).await;
                return;
            }
            Err(e) => {
                let error = format!("failed to read the backfill queue: {e}");
                fail_job(&state, &job_id, &error).await;
                return;
            }
        }
    }

    complete_job(&state, &job_id, final_processed, final_records, None).await;

    state
        .host
        .log_event(ProviderEvent {
            event_type: "backfill.completed".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: collection,
            detail: serde_json::json!({
                "job_id": job_id,
                "total_repos": final_processed,
                "total_records": final_records,
            }),
        })
        .await;
}

/// Pause a bounded job that ran out of work to do with units still queued,
/// rather than mark it completed with work left.
///
/// The reason goes in the job's `error`, so the dashboard can tell this from
/// a pause an operator asked for. Resuming clears it.
pub(crate) async fn pause_with_units_left(
    state: &JobState,
    job_id: &str,
    collection: Option<&str>,
    left: i64,
) {
    let reason = format!("paused: {left} units could not be completed; resume to retry");
    tracing::warn!(job_id, left, "{reason}");
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'paused', error = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "pause", || {
        happyview_provider::db::query(&sql)
            .bind(&reason)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "paused".to_string(),
            error: Some(reason.clone()),
        },
    );
    state
        .host
        .log_event(ProviderEvent {
            event_type: "backfill.units_left".to_string(),
            severity: Severity::Warn,
            actor_did: None,
            subject: collection.map(str::to_string),
            detail: serde_json::json!({
                "job_id": job_id,
                "queued_units": left,
                "reason": reason,
            }),
        })
        .await;
}

/// Insert a job whose repos are known up front, together with a unit per account.
///
/// The job row and its repos must appear together or not at all: a failure
/// partway through the chunked insert must not leave behind a job marked
/// `running` with no worker and no (or partial) repos to work on,
/// indistinguishable from a live job until the next restart's
/// `resume_backfill_jobs` sweep notices it. Callers spawn the worker only
/// after this returns, since a worker started inside the transaction could
/// observe rows that later roll back.
///
/// `discovery_complete = 1` makes `run_backfill_job` skip discovery, both on
/// first run and on resume.
pub(crate) async fn insert_targeted_job(
    state: &JobState,
    job_id: &str,
    collection: Option<&str>,
    dids: &[String],
    source: SourceChoice,
) -> Result<(), AdminError> {
    let backend = state.db_backend;
    let now = now_rfc3339();
    let single_did = match dids {
        [did] => Some(did.as_str()),
        _ => None,
    };
    let total = i32::try_from(dids.len()).unwrap_or(i32::MAX);

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| AdminError::Internal(format!("failed to begin transaction: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, queue_version, discovery_complete, started_at, created_at, source) \
         VALUES (?, ?, ?, 'dids', 'running', 'resolving_pds', ?, 2, 1, ?, ?, ?)",
        backend,
    );
    happyview_provider::db::query(&sql)
        .bind(job_id)
        .bind(collection)
        .bind(single_did)
        .bind(total)
        .bind(&now)
        .bind(&now)
        .bind(source.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| AdminError::Internal(format!("failed to create backfill job: {e}")))?;

    for chunk in dids.chunks(DISCOVERY_INSERT_CHUNK) {
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql_str = format!(
            "INSERT INTO happyview_backfill_queue (job_id, collection, did) VALUES {placeholders} ON CONFLICT DO NOTHING",
        );
        let sql = adapt_sql(&sql_str, backend);
        let mut insert = happyview_provider::db::query(&sql);
        for did in chunk {
            insert = insert.bind(job_id).bind(ALL_COLLECTIONS).bind(did);
        }
        insert
            .execute(&mut *tx)
            .await
            .map_err(|e| AdminError::Internal(format!("failed to seed backfill units: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AdminError::Internal(format!("failed to commit backfill job: {e}")))
}

/// `create_backfill_job_with` at `BACKFILL_SOURCE`. Only tests create a job
/// without starting it at the default source.
pub async fn create_backfill_job(
    state: &JobState,
    collection: Option<&str>,
    dids: &[String],
) -> Result<String, AdminError> {
    create_backfill_job_with(state, collection, dids, state.archive.default_source).await
}

/// Insert a backfill job without starting it. With no `dids` the job
/// discovers repos through the relay; otherwise it targets exactly those.
pub(crate) async fn create_backfill_job_with(
    state: &JobState,
    collection: Option<&str>,
    dids: &[String],
    source: SourceChoice,
) -> Result<String, AdminError> {
    let job_id = Uuid::new_v4().to_string();

    if !dids.is_empty() {
        insert_targeted_job(state, &job_id, collection, dids, source).await?;
        return Ok(job_id);
    }

    insert_network_job(state, &state.db, &job_id, collection, source, None).await?;
    Ok(job_id)
}

/// Create a network replay job for everything after `after_seq` (a gap in
/// the live stream). `replay_after_seq` goes in with the row, so no reader,
/// including a restart's resume, ever sees the job without it.
pub async fn create_gap_replay_job(state: &JobState, after_seq: i64) -> Result<String, AdminError> {
    let job_id = Uuid::new_v4().to_string();
    insert_network_job(
        state,
        &state.backfill_db,
        &job_id,
        None,
        SourceChoice::Replay,
        Some(after_seq),
    )
    .await?;
    Ok(job_id)
}

pub(crate) async fn insert_network_job(
    state: &JobState,
    pool: &sqlx::AnyPool,
    job_id: &str,
    collection: Option<&str>,
    source: SourceChoice,
    replay_after_seq: Option<i64>,
) -> Result<(), AdminError> {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, queue_version, discovery_complete, started_at, created_at, source, replay_after_seq) \
         VALUES (?, ?, NULL, 'network', 'running', 'pending', 0, 2, 0, ?, ?, ?, ?)",
        state.db_backend,
    );
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&sql)
            .bind(job_id)
            .bind(collection)
            .bind(&now)
            .bind(&now)
            .bind(source.as_str())
            .bind(replay_after_seq)
            .execute(pool)
    })
    .await
    .map_err(|e| AdminError::Internal(format!("failed to create backfill job: {e}")))?;
    Ok(())
}

pub(crate) async fn start_backfill(
    state: &JobState,
    collection: Option<String>,
    dids: Vec<String>,
    actor_did: &str,
) -> Result<String, AdminError> {
    start_backfill_with(state, collection, dids, actor_did, None).await
}

/// `start_backfill` with the source asked for; `None`: `BACKFILL_SOURCE`.
pub(crate) async fn start_backfill_with(
    state: &JobState,
    collection: Option<String>,
    dids: Vec<String>,
    actor_did: &str,
    requested: Option<SourceChoice>,
) -> Result<String, AdminError> {
    let source = requested.unwrap_or(state.archive.default_source);
    // Account backfills always crawl, whatever the default (see
    // `create_backfill`).
    let source = if dids.is_empty() {
        source
    } else {
        SourceChoice::Crawl
    };
    // A collection with an executor is backfilled by it.
    if dids.is_empty()
        && let Some(col) = collection.as_deref()
        && let Some(executor) = state.compose.executor(col)
    {
        if let Some(requested) = requested {
            tracing::info!(
                collection = col,
                requested = requested.as_str(),
                executor = executor.name(),
                "this collection is backfilled by its executor, not by the source asked for"
            );
        }
        return start_executor_job(state, col, executor, actor_did).await;
    }
    let job_id = create_backfill_job_with(state, collection.as_deref(), &dids, source).await?;
    let scope = if dids.is_empty() { "network" } else { "dids" };

    state
        .host
        .log_event(ProviderEvent {
            event_type: "backfill.started".to_string(),
            severity: Severity::Info,
            actor_did: Some(actor_did.to_string()),
            subject: collection.clone(),
            detail: serde_json::json!({
                "job_id": job_id.clone(),
                "scope": scope,
                "account_count": dids.len(),
                "source": source.as_str(),
            }),
        })
        .await;

    spawn_job(state, &job_id, collection.clone());

    // A network backfill of every collection leaves the collections with an
    // executor out (see `run_backfill_job_with`); each gets its executor's
    // own job.
    if collection.is_none() && dids.is_empty() && state.compose.executor_for.is_some() {
        let tracked = state.host.tracked_collections().borrow().clone();
        let mut executor_jobs = Vec::new();
        for col in tracked {
            let Some(executor) = state.compose.executor(&col) else {
                continue;
            };
            match start_executor_job(state, &col, executor, actor_did).await {
                Ok(id) => executor_jobs.push(id),
                Err(e) => {
                    tracing::warn!(collection = %col, error = %e, "could not start an executor backfill")
                }
            }
        }
        if !executor_jobs.is_empty() {
            tracing::info!(
                job_id,
                executor_jobs = ?executor_jobs,
                "started executor backfills beside this backfill of every collection"
            );
        }
    }

    Ok(job_id)
}

// ---------------------------------------------------------------------------
// Executor jobs
// ---------------------------------------------------------------------------

/// Create and spawn a job of `executor` backfilling `collection`. Its
/// `source` is the executor's name.
pub(crate) async fn start_executor_job(
    state: &JobState,
    collection: &str,
    executor: Arc<dyn BackfillExecutor>,
    actor_did: &str,
) -> Result<String, AdminError> {
    let name = executor.name();
    if crate::archive::is_builtin_source(name) || name.is_empty() {
        return Err(AdminError::Internal(format!(
            "a backfill executor cannot be named {name:?}"
        )));
    }
    let job_id = Uuid::new_v4().to_string();
    let now = now_rfc3339();
    // Nothing to discover: `discovery_complete = 1`.
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, queue_version, discovery_complete, started_at, created_at, source) \
         VALUES (?, ?, NULL, 'network', 'running', 'pending', 0, 2, 1, ?, ?, ?)",
        state.db_backend,
    );
    happyview_provider::db::retry_on_busy(|| {
        happyview_provider::db::query(&sql)
            .bind(&job_id)
            .bind(collection)
            .bind(&now)
            .bind(&now)
            .bind(name)
            .execute(&state.db)
    })
    .await
    .map_err(|e| AdminError::Internal(format!("failed to create backfill job: {e}")))?;

    state
        .host
        .log_event(ProviderEvent {
            event_type: "backfill.started".to_string(),
            severity: Severity::Info,
            actor_did: Some(actor_did.to_string()),
            subject: Some(collection.to_string()),
            detail: serde_json::json!({
                "job_id": job_id.clone(),
                "scope": "network",
                "account_count": 0,
                "source": name,
            }),
        })
        .await;

    spawn_job(state, &job_id, Some(collection.to_string()));
    Ok(job_id)
}

/// Run an executor's job: the executor `name` that backfills `collection`.
/// One no longer configured fails the job; nothing else runs it.
async fn run_executor_job(state: &JobState, job_id: &str, collection: Option<&str>, name: &str) {
    let Some(collection) = collection else {
        fail_job(
            state,
            job_id,
            &format!("backfill executor '{name}' has no collection to backfill"),
        )
        .await;
        return;
    };
    let Some(executor) = state
        .compose
        .executor(collection)
        .filter(|executor| executor.name() == name)
    else {
        fail_job(
            state,
            job_id,
            &format!(
                "backfill executor '{name}' is not configured for {collection}; start a new backfill"
            ),
        )
        .await;
        return;
    };

    let handle = JobHandle::new(state.clone(), job_id, collection).await;
    let run = AssertUnwindSafe(executor.run(handle.clone(), collection))
        .catch_unwind()
        .await;
    handle.flush_errors().await;
    let Ok(result) = run else {
        tracing::error!(job_id, executor = name, "backfill executor panicked");
        fail_job(
            state,
            job_id,
            &format!("backfill executor '{name}' panicked"),
        )
        .await;
        return;
    };
    // `total_records` sums every run of the job.
    let records = result
        .as_ref()
        .map(|totals| handle.records_with(totals.records))
        .ok();
    match should_stop(state, job_id).await {
        Some(stop) => {
            if let Some(records) = records {
                update_job_counter(state, job_id, "total_records", records).await;
            }
            if stop == "cancelling" {
                tracing::info!(job_id, "backfill job cancelled");
                finalise_cancel(state, job_id).await;
            } else {
                tracing::info!(job_id, "backfill job paused");
                finalise_pause(state, job_id).await;
            }
        }
        None => match result {
            Ok(_) => {
                let records = records.unwrap_or_default();
                complete_job(state, job_id, handle.processed(), records, None).await;
                state
                    .host
                    .log_event(ProviderEvent {
                        event_type: "backfill.completed".to_string(),
                        severity: Severity::Info,
                        actor_did: None,
                        subject: Some(collection.to_string()),
                        detail: serde_json::json!({
                            "job_id": job_id,
                            "source": name,
                            "total_records": records,
                        }),
                    })
                    .await;
            }
            Err(error) => {
                tracing::error!(job_id, executor = name, error, "executor backfill failed");
                fail_job(state, job_id, &error).await;
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Flush endpoints
// ---------------------------------------------------------------------------

/// Per-job detail a flush or the retention sweep clears: a legacy job's repo
/// rows, and a bounded job's recent completions and PDS summary.
pub(crate) const BACKFILL_DETAIL_TABLES: [&str; 3] = [
    "happyview_backfill_repos",
    "happyview_backfill_completions",
    "happyview_backfill_pds_stats",
];

/// A bounded job's live work. A running or paused job still needs it, so it
/// is cleared only once the job has finished. A cancelled or failed job can
/// leave up to a window of units behind.
pub(crate) const BACKFILL_WORK_TABLES: [&str; 2] =
    ["happyview_backfill_queue", "happyview_backfill_cursors"];

/// Statuses a job never leaves.
pub const FINISHED_STATUSES: [&str; 3] = ["completed", "cancelled", "failed"];

pub(crate) const FINISHED_JOBS: &str =
    "SELECT id FROM happyview_backfill_jobs WHERE status IN ('completed', 'cancelled', 'failed')";

/// Rows each flush or retention DELETE removes at a time. A legacy network
/// job can hold millions of repo rows, and one statement over all of them
/// would hold the write lock (and on SQLite grow the WAL) for its whole run.
pub(crate) const BACKFILL_DELETE_BATCH: i64 = 10_000;

/// Delete the rows of `table` matching `filter`, `batch` at a time, each
/// batch its own statement retried while the database is busy. Returns the
/// rows deleted.
pub(crate) async fn delete_detail_rows(
    state: &JobState,
    table: &str,
    filter: &str,
    binds: &[&str],
    batch: i64,
) -> Result<u64, AdminError> {
    // Neither backend has DELETE … LIMIT, so each batch picks its rows by
    // physical row id.
    let row_id = match state.db_backend {
        happyview_provider::db::DatabaseBackend::Sqlite => "rowid",
        happyview_provider::db::DatabaseBackend::Postgres => "ctid",
    };
    let sql = adapt_sql(
        &format!(
            "DELETE FROM {table} WHERE {row_id} IN \
             (SELECT {row_id} FROM {table} WHERE {filter} LIMIT ?)"
        ),
        state.db_backend,
    );
    let mut deleted = 0;
    loop {
        let rows = happyview_provider::db::retry_on_busy(|| {
            let mut q = happyview_provider::db::query(&sql);
            for value in binds {
                q = q.bind(*value);
            }
            q.bind(batch).execute(&state.backfill_db)
        })
        .await
        .map(|result| result.rows_affected())
        .map_err(|e| AdminError::Internal(format!("failed to flush backfill details: {e}")))?;
        deleted += rows;
        if rows < batch as u64 {
            return Ok(deleted);
        }
    }
}

/// Delete one job's detail rows, and its queue if it has finished.
pub async fn flush_job_details(
    state: &JobState,
    job_id: &str,
    batch: i64,
) -> Result<(), AdminError> {
    for table in BACKFILL_DETAIL_TABLES {
        delete_detail_rows(state, table, "job_id = ?", &[job_id], batch).await?;
    }
    let finished = format!("job_id = ? AND job_id IN ({FINISHED_JOBS})");
    for table in BACKFILL_WORK_TABLES {
        delete_detail_rows(state, table, &finished, &[job_id], batch).await?;
    }
    Ok(())
}

pub async fn flush_finished_details(state: &JobState, batch: i64) -> Result<(), AdminError> {
    let filter = format!("job_id IN ({FINISHED_JOBS})");
    for table in BACKFILL_DETAIL_TABLES.iter().chain(&BACKFILL_WORK_TABLES) {
        delete_detail_rows(state, table, &filter, &[], batch).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Retention cleanup
// ---------------------------------------------------------------------------

/// Delete every detail and leftover queue row of jobs that finished before
/// `cutoff` (RFC 3339). Returns the rows deleted.
pub async fn sweep_backfill_details(state: &JobState, cutoff: &str) -> Result<u64, AdminError> {
    sweep_backfill_details_in_batches(state, cutoff, BACKFILL_DELETE_BATCH).await
}

pub async fn sweep_backfill_details_in_batches(
    state: &JobState,
    cutoff: &str,
    batch: i64,
) -> Result<u64, AdminError> {
    let filter =
        format!("job_id IN ({FINISHED_JOBS} AND completed_at IS NOT NULL AND completed_at < ?)");
    let mut deleted = 0;
    for table in BACKFILL_DETAIL_TABLES.iter().chain(&BACKFILL_WORK_TABLES) {
        deleted += delete_detail_rows(state, table, &filter, &[cutoff], batch).await?;
    }
    Ok(deleted)
}

pub(crate) async fn run_backfill_retention_cleanup(state: &JobState) {
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400));

    loop {
        interval.tick().await;

        let retention_days: i64 = state
            .host
            .setting("backfill_retention_days")
            .await
            .and_then(|v| v.parse().ok())
            .unwrap_or(28);

        if retention_days == 0 {
            continue;
        }

        let cutoff = (chrono::Utc::now() - chrono::Duration::days(retention_days)).to_rfc3339();
        match sweep_backfill_details(state, &cutoff).await {
            Ok(0) => {}
            Ok(deleted) => {
                tracing::info!(
                    deleted,
                    retention_days,
                    "cleaned up old backfill detail rows"
                );
            }
            Err(e) => tracing::warn!(error = %e, "backfill retention cleanup failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Startup resumption
// ---------------------------------------------------------------------------

/// Resume any backfill jobs that were running when the server last stopped.
/// Jobs stuck in `cancelling` are finalised immediately.
pub async fn resume_backfill_jobs(state: &JobState) {
    let sql = adapt_sql(
        "SELECT id, status, collection FROM happyview_backfill_jobs WHERE status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let rows: Vec<(String, String, Option<String>)> = happyview_provider::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    for (job_id, status, collection) in rows {
        match status.as_str() {
            "cancelling" => {
                tracing::info!(
                    job_id,
                    "finalising cancelled backfill job from previous run"
                );
                finalise_cancel(state, &job_id).await;
            }
            "pausing" => {
                tracing::info!(job_id, "finalising paused backfill job from previous run");
                finalise_pause(state, &job_id).await;
            }
            _ => {
                tracing::info!(job_id, "resuming interrupted backfill job");
                spawn_job(state, &job_id, collection);
            }
        }
    }
}

#[cfg(test)]
mod running_tests {
    use super::RunningJobs;

    #[test]
    fn a_job_runs_until_its_last_guard_goes() {
        let running = RunningJobs::default();
        assert!(!running.covers("app.a"));
        let outer = running.enter("j1", Some("app.a".into()));
        let inner = running.enter("j1", Some("app.a".into()));
        assert!(running.covers("app.a"));
        assert!(!running.covers("app.b"));
        drop(inner);
        assert!(running.covers("app.a"));
        drop(outer);
        assert!(!running.covers("app.a"));
    }

    #[test]
    fn a_job_without_a_collection_covers_every_one() {
        let running = RunningJobs::default();
        let _all = running.enter("j2", None);
        assert!(running.covers("app.a"));
        assert!(running.covers("app.b"));
    }
}
