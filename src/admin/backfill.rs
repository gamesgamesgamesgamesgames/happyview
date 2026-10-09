use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use futures_util::FutureExt;
use futures_util::stream::{self, FuturesUnordered, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;
use uuid::Uuid;

use rand::RngExt;

use crate::AppState;
use crate::db::{adapt_sql, now_rfc3339};
use crate::error::AppError;
use crate::event_log::{EventLog, Severity, log_event};
use crate::http_retry::parse_retry_after;
use crate::profile;

use super::auth::UserAuth;
use super::backfill_errors::{BackfillErrorKind, ERROR_DETAIL_CAP, ErrorCounts};
use super::backfill_retry::{
    DeferredItem, DeferredQueue, DrainStep, HostCooldowns, next_drain_step,
};
use super::permissions::Permission;
use super::types::{
    BackfillErrorCount, BackfillErrorEntry, BackfillErrorsResponse, BackfillJob, CreateBackfillBody,
};

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ListReposResponse {
    repos: Vec<RepoEntry>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RepoEntry {
    did: String,
}

#[derive(Deserialize)]
struct ListRecordsResponse {
    records: Vec<RecordEntry>,
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RecordEntry {
    uri: String,
    cid: String,
    value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn set_stage(state: &AppState, job_id: &str, stage: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET stage = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "stage", || {
        crate::db::query(&sql)
            .bind(stage)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobStageChanged {
            job_id: job_id.to_string(),
            stage: stage.to_string(),
        },
    );
}

async fn update_job_counter(state: &AppState, job_id: &str, column: &str, value: i32) {
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
        crate::db::query(&sql)
            .bind(value)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

fn publish_event(state: &AppState, event: super::types::BackfillEvent) {
    let _ = state.backfill_events_tx.send(event);
}

/// Run one bookkeeping write for a job, retrying while the database is busy.
/// A write that still fails is logged and recorded in the event log rather
/// than dropped, so a counter or stage that stopped moving can be traced.
pub(super) async fn job_write<T, F, Fut>(
    state: &AppState,
    job_id: &str,
    what: &'static str,
    op: F,
) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, sqlx::Error>>,
{
    match crate::db::retry_on_busy(op).await {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::error!(job_id, what, error = %e, "backfill bookkeeping write failed");
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.write_failed".to_string(),
                    severity: Severity::Error,
                    actor_did: None,
                    subject: Some(job_id.to_string()),
                    detail: serde_json::json!({
                        "job_id": job_id,
                        "write": what,
                        "error": e.to_string(),
                    }),
                },
                state.db_backend,
            )
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
async fn build_job_snapshot(state: &AppState, job_id: &str) -> Option<super::types::BackfillEvent> {
    let sql = adapt_sql(
        "SELECT status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts \
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
    )> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let (status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts) =
        row?;

    Some(super::types::BackfillEvent::JobSnapshot {
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
    })
}

fn random_batch_threshold(base: i32) -> i32 {
    let low = base - base / 10;
    rand::rng().random_range(low..=base)
}

struct BackfillConcurrency {
    resolution: usize,
    pds: usize,
    dids_per_pds: usize,
}

async fn load_concurrency(state: &AppState) -> BackfillConcurrency {
    let resolution = super::settings::get_setting(
        &state.db,
        "backfill_concurrent_resolution",
        state.db_backend,
    )
    .await
    .and_then(|v| v.parse().ok())
    .unwrap_or(100usize)
    .max(1);
    let pds = super::settings::get_setting(&state.db, "backfill_concurrent_pds", state.db_backend)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(10usize)
        .max(1);
    let dids_per_pds = super::settings::get_setting(
        &state.db,
        "backfill_concurrent_dids_per_pds",
        state.db_backend,
    )
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

async fn load_max_attempts(state: &AppState) -> u32 {
    super::settings::get_setting(&state.db, "backfill_max_attempts", state.db_backend)
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(3u32)
        .clamp(1, 10)
}

async fn fail_job(state: &AppState, job_id: &str, error: &str) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'failed', completed_at = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "fail", || {
        crate::db::query(&sql)
            .bind(&now)
            .bind(error)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "failed".to_string(),
            error: Some(error.to_string()),
        },
    );
}

async fn should_stop(state: &AppState, job_id: &str) -> Option<&'static str> {
    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let status = crate::db::query_as::<(String,)>(&sql)
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

async fn should_stop_worker(state: &AppState, job_id: &str) -> bool {
    should_stop(state, job_id).await.is_some()
}

async fn request_cancel(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelling' WHERE id = ? AND status IN ('running', 'paused')",
        state.db_backend,
    );
    job_write(state, job_id, "request_cancel", || {
        crate::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

async fn finalise_cancel(state: &AppState, job_id: &str) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'cancelled', completed_at = ?, error = 'cancelled by user' WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "cancel", || {
        crate::db::query(&sql)
            .bind(&now)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "cancelled".to_string(),
            error: None,
        },
    );
}

async fn request_pause(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'pausing' WHERE id = ? AND status = 'running'",
        state.db_backend,
    );
    job_write(state, job_id, "request_pause", || {
        crate::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
}

async fn finalise_pause(state: &AppState, job_id: &str) {
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'paused' WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "pause", || {
        crate::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "paused".to_string(),
            error: None,
        },
    );
}

async fn complete_job(
    state: &AppState,
    job_id: &str,
    processed_repos: i32,
    total_records: i32,
    error: Option<&str>,
) {
    let now = now_rfc3339();
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET status = 'completed', stage = 'completed', completed_at = ?, processed_repos = ?, total_records = ?, error = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "complete", || {
        crate::db::query(&sql)
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
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "completed".to_string(),
            error: error.map(|e| e.to_string()),
        },
    );
}

// ---------------------------------------------------------------------------
// Work queue
// ---------------------------------------------------------------------------

/// Pending units a job may hold at once unless `BACKFILL_DISCOVERY_WINDOW`
/// says otherwise.
pub const DEFAULT_DISCOVERY_WINDOW: i64 = 50_000;

/// Completed units a bounded job keeps for
/// `GET /admin/backfill/{id}/repos?phase=fetched`.
pub const RECENT_COMPLETIONS_PER_JOB: i64 = 1000;

/// The collection of a unit that covers every collection its job targets:
/// account-targeted jobs, and every job that predates the bounded queue.
const ALL_COLLECTIONS: &str = "";

/// The largest relay page discovery asks for.
const RELAY_PAGE_LIMIT: i64 = 1000;

/// Discovered DIDs per INSERT: three bound parameters each, under SQLite's 999.
const DISCOVERY_INSERT_CHUNK: usize = 300;

/// Rows per DELETE when a pre-upgrade job's partial repo list is discarded,
/// so millions of rows never go through one transaction.
const LEGACY_DELETE_BATCH: i64 = 5000;

/// Completed units between trims of a job's recent-completions log. Trimming
/// inside every unit's transaction would add a ranged DELETE to each one, so
/// the log may run up to this many rows over `RECENT_COMPLETIONS_PER_JOB`
/// between trims.
const COMPLETIONS_TRIM_EVERY: i32 = 100;

/// Units the resolver reads per query. Paging bounds the resolver's memory
/// and keeps each read short, where one `fetch_all` held every unresolved DID
/// (6.86M on one tenant) and a read snapshot open for the whole scan.
const RESOLVE_PAGE_SIZE: i64 = 1000;

/// Parse `BACKFILL_DISCOVERY_WINDOW`. Anything unparseable or below 1 falls
/// back to the default rather than failing a job over a tuning knob.
pub fn parse_discovery_window(raw: Option<&str>) -> i64 {
    raw.and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n >= 1)
        .unwrap_or(DEFAULT_DISCOVERY_WINDOW)
}

/// Where a job keeps its work. `Legacy` jobs were created before the bounded
/// queue: their repos were all discovered up front into
/// `happyview_backfill_repos`, and they finish there. `Bounded` jobs queue
/// units in `happyview_backfill_queue`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueVersion {
    Legacy,
    Bounded,
}

impl QueueVersion {
    fn from_column(value: i32) -> Self {
        if value >= 2 {
            Self::Bounded
        } else {
            Self::Legacy
        }
    }
}

/// One unit of fetch work: a repo, and the collection it was discovered under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct WorkUnit {
    did: String,
    /// `ALL_COLLECTIONS` for every collection the job targets.
    collection: String,
}

impl WorkUnit {
    fn all_collections(did: String) -> Self {
        Self {
            did,
            collection: ALL_COLLECTIONS.to_string(),
        }
    }

    /// The collections to fetch for this unit.
    fn collections(&self, job_collections: &[String]) -> Vec<String> {
        if self.collection == ALL_COLLECTIONS {
            job_collections.to_vec()
        } else {
            vec![self.collection.clone()]
        }
    }
}

/// How many units a job has queued, against its cap.
///
/// Discovery reserves room for a whole relay page before fetching it, so
/// collections discovered concurrently cannot overshoot the cap between them.
/// It then settles the reservation against what it actually enqueued.
#[derive(Debug)]
struct QueueWindow {
    capacity: i64,
    queued: AtomicI64,
}

impl QueueWindow {
    fn new(capacity: i64, queued: i64) -> Self {
        Self {
            capacity: capacity.max(1),
            queued: AtomicI64::new(queued),
        }
    }

    fn capacity(&self) -> i64 {
        self.capacity
    }

    /// Claim `n` slots if they fit.
    fn try_reserve(&self, n: i64) -> bool {
        let mut current = self.queued.load(Ordering::Acquire);
        loop {
            if current + n > self.capacity {
                return false;
            }
            match self.queued.compare_exchange_weak(
                current,
                current + n,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => current = actual,
            }
        }
    }

    /// Trade a reservation of `reserved` slots for the `used` actually taken.
    fn settle(&self, reserved: i64, used: i64) {
        self.queued.fetch_add(used - reserved, Ordering::AcqRel);
    }

    /// Units left the queue.
    fn release(&self, n: i64) {
        self.queued.fetch_sub(n, Ordering::AcqRel);
    }
}

/// A job's queue as the pipeline sees it.
#[derive(Clone)]
struct JobQueue {
    job_id: Arc<String>,
    version: QueueVersion,
    window: Arc<QueueWindow>,
    /// Set once discovery can enqueue nothing more. Until then the resolver
    /// keeps looking for new units instead of finishing.
    discovery_done: Arc<AtomicBool>,
}

impl JobQueue {
    /// A legacy job: its repo list was complete before it started, and it has
    /// no window.
    fn legacy(job_id: &str) -> Self {
        Self {
            job_id: Arc::new(job_id.to_string()),
            version: QueueVersion::Legacy,
            window: Arc::new(QueueWindow::new(i64::MAX, 0)),
            discovery_done: Arc::new(AtomicBool::new(true)),
        }
    }
}

/// Sets `discovery_done` however discovery ends, panics included, so the
/// resolver is never left waiting on a discovery that is gone.
struct MarkDiscoveryDone(Arc<AtomicBool>);

impl Drop for MarkDiscoveryDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The next page of a job's unresolved units after `after`. Legacy jobs page
/// by DID; bounded jobs by `(collection, did)`, their primary key order.
async fn unresolved_page(
    state: &AppState,
    queue: &JobQueue,
    after: Option<&WorkUnit>,
) -> Result<Vec<WorkUnit>, sqlx::Error> {
    let job_id = queue.job_id.as_str();
    match queue.version {
        QueueVersion::Legacy => {
            let sql = adapt_sql(
                "SELECT did FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NULL AND did > ? ORDER BY did LIMIT ?",
                state.db_backend,
            );
            let rows: Vec<(String,)> = crate::db::query_as(&sql)
                .bind(job_id)
                .bind(after.map_or("", |unit| unit.did.as_str()))
                .bind(RESOLVE_PAGE_SIZE)
                .fetch_all(&state.backfill_db)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(did,)| WorkUnit::all_collections(did))
                .collect())
        }
        QueueVersion::Bounded => {
            let (after_collection, after_did) = after.map_or(("", ""), |unit| {
                (unit.collection.as_str(), unit.did.as_str())
            });
            let sql = adapt_sql(
                "SELECT collection, did FROM happyview_backfill_queue \
                 WHERE job_id = ? AND pds_endpoint IS NULL AND (collection > ? OR (collection = ? AND did > ?)) \
                 ORDER BY collection, did LIMIT ?",
                state.db_backend,
            );
            let rows: Vec<(String, String)> = crate::db::query_as(&sql)
                .bind(job_id)
                .bind(after_collection)
                .bind(after_collection)
                .bind(after_did)
                .bind(RESOLVE_PAGE_SIZE)
                .fetch_all(&state.backfill_db)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(collection, did)| WorkUnit { did, collection })
                .collect())
        }
    }
}

/// The next page, after `after`, of units an earlier run resolved but did not
/// fetch, with their PDS. Paged in the same order as `unresolved_page`, since
/// a legacy job can hold millions of them.
async fn resolved_page(
    state: &AppState,
    queue: &JobQueue,
    after: Option<&WorkUnit>,
) -> Result<Vec<(WorkUnit, String)>, sqlx::Error> {
    let job_id = queue.job_id.as_str();
    match queue.version {
        QueueVersion::Legacy => {
            let sql = adapt_sql(
                "SELECT did, pds_endpoint FROM happyview_backfill_repos \
                 WHERE job_id = ? AND status = 'pending' AND pds_endpoint IS NOT NULL AND did > ? \
                 ORDER BY did LIMIT ?",
                state.db_backend,
            );
            let rows: Vec<(String, String)> = crate::db::query_as(&sql)
                .bind(job_id)
                .bind(after.map_or("", |unit| unit.did.as_str()))
                .bind(RESOLVE_PAGE_SIZE)
                .fetch_all(&state.backfill_db)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(did, pds)| (WorkUnit::all_collections(did), pds))
                .collect())
        }
        QueueVersion::Bounded => {
            let (after_collection, after_did) = after.map_or(("", ""), |unit| {
                (unit.collection.as_str(), unit.did.as_str())
            });
            let sql = adapt_sql(
                "SELECT collection, did, pds_endpoint FROM happyview_backfill_queue \
                 WHERE job_id = ? AND pds_endpoint IS NOT NULL AND (collection > ? OR (collection = ? AND did > ?)) \
                 ORDER BY collection, did LIMIT ?",
                state.db_backend,
            );
            let rows: Vec<(String, String, String)> = crate::db::query_as(&sql)
                .bind(job_id)
                .bind(after_collection)
                .bind(after_collection)
                .bind(after_did)
                .bind(RESOLVE_PAGE_SIZE)
                .fetch_all(&state.backfill_db)
                .await?;
            Ok(rows
                .into_iter()
                .map(|(collection, did, pds)| (WorkUnit { did, collection }, pds))
                .collect())
        }
    }
}

/// Record a unit's PDS, the job's resolved counter and, for a bounded job,
/// the PDS's share of the job, in one transaction.
async fn mark_resolved(
    state: &AppState,
    queue: &JobQueue,
    unit: &WorkUnit,
    pds: &str,
) -> Result<(), sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let mut tx = state.backfill_db.begin().await?;
    match queue.version {
        QueueVersion::Legacy => {
            let sql = adapt_sql(
                "UPDATE happyview_backfill_repos SET pds_endpoint = ? WHERE job_id = ? AND did = ?",
                backend,
            );
            crate::db::query(&sql)
                .bind(pds)
                .bind(job_id)
                .bind(&unit.did)
                .execute(&mut *tx)
                .await?;
        }
        QueueVersion::Bounded => {
            let sql = adapt_sql(
                "UPDATE happyview_backfill_queue SET pds_endpoint = ? WHERE job_id = ? AND collection = ? AND did = ?",
                backend,
            );
            crate::db::query(&sql)
                .bind(pds)
                .bind(job_id)
                .bind(&unit.collection)
                .bind(&unit.did)
                .execute(&mut *tx)
                .await?;
            let stats = adapt_sql(
                "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, repos) VALUES (?, ?, 1) \
                 ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET repos = happyview_backfill_pds_stats.repos + 1",
                backend,
            );
            crate::db::query(&stats)
                .bind(job_id)
                .bind(pds)
                .execute(&mut *tx)
                .await?;
        }
    }
    let counter = adapt_sql(
        "UPDATE happyview_backfill_jobs SET resolved_repos = COALESCE(resolved_repos, 0) + 1 WHERE id = ?",
        backend,
    );
    crate::db::query(&counter)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// Complete a unit in one transaction: delete it from the queue, log it among
/// the job's recent completions, count it against its PDS, and bump the job's
/// counters. A legacy row is marked completed, as before. Returns whether a
/// queued row was removed.
///
/// The records were written page by page before this runs, so a crash in
/// between leaves the unit queued and a resumed job fetches it again; the
/// pages it rewrites are no-op upserts.
async fn complete_unit(
    state: &AppState,
    queue: &JobQueue,
    unit: &WorkUnit,
    pds: &str,
    records: i32,
) -> Result<bool, sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let mut tx = state.backfill_db.begin().await?;
    let removed = match queue.version {
        QueueVersion::Legacy => {
            let sql = adapt_sql(
                "UPDATE happyview_backfill_repos SET status = 'completed', records_fetched = ? WHERE job_id = ? AND did = ?",
                backend,
            );
            crate::db::query(&sql)
                .bind(records)
                .bind(job_id)
                .bind(&unit.did)
                .execute(&mut *tx)
                .await?;
            false
        }
        QueueVersion::Bounded => {
            let delete = adapt_sql(
                "DELETE FROM happyview_backfill_queue WHERE job_id = ? AND collection = ? AND did = ?",
                backend,
            );
            let removed = crate::db::query(&delete)
                .bind(job_id)
                .bind(&unit.collection)
                .bind(&unit.did)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                > 0;
            let log = adapt_sql(
                "INSERT INTO happyview_backfill_completions (job_id, did, collection, pds_endpoint, records_fetched) VALUES (?, ?, ?, ?, ?)",
                backend,
            );
            crate::db::query(&log)
                .bind(job_id)
                .bind(&unit.did)
                .bind(&unit.collection)
                .bind(pds)
                .bind(records)
                .execute(&mut *tx)
                .await?;
            let stats = adapt_sql(
                "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, completed_repos, records) VALUES (?, ?, 1, ?) \
                 ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET \
                 completed_repos = happyview_backfill_pds_stats.completed_repos + 1, \
                 records = happyview_backfill_pds_stats.records + excluded.records",
                backend,
            );
            crate::db::query(&stats)
                .bind(job_id)
                .bind(pds)
                .bind(i64::from(records))
                .execute(&mut *tx)
                .await?;
            removed
        }
    };
    let counters = adapt_sql(
        "UPDATE happyview_backfill_jobs SET processed_repos = COALESCE(processed_repos, 0) + 1, \
         total_records = COALESCE(total_records, 0) + ? WHERE id = ?",
        backend,
    );
    crate::db::query(&counters)
        .bind(records)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(removed)
}

/// Trim a job's recent-completions log to its newest
/// `RECENT_COMPLETIONS_PER_JOB` rows.
async fn trim_completions(state: &AppState, job_id: &str) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_completions WHERE job_id = ? AND id <= \
         (SELECT id FROM happyview_backfill_completions WHERE job_id = ? ORDER BY id DESC LIMIT 1 OFFSET ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(job_id)
        .bind(RECENT_COMPLETIONS_PER_JOB)
        .execute(&state.backfill_db)
        .await
        .map(|_| ())
}

/// Trim a bounded job's completions log once every `COMPLETIONS_TRIM_EVERY`
/// completed units, `processed` being the job's count so far.
async fn trim_completions_on_schedule(state: &AppState, queue: &JobQueue, processed: i32) {
    if queue.version != QueueVersion::Bounded || processed % COMPLETIONS_TRIM_EVERY != 0 {
        return;
    }
    job_write(state, &queue.job_id, "trim_completions", || {
        trim_completions(state, &queue.job_id)
    })
    .await;
}

/// Remove a unit that cannot be resolved; its failure is in
/// `happyview_backfill_errors`, which retry-failed reads. A legacy row stays,
/// as it always did, so a resumed job tries it again. Returns whether a row
/// left the queue.
async fn drop_unresolvable(
    state: &AppState,
    queue: &JobQueue,
    unit: &WorkUnit,
) -> Result<bool, sqlx::Error> {
    if queue.version == QueueVersion::Legacy {
        return Ok(false);
    }
    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_queue WHERE job_id = ? AND collection = ? AND did = ?",
        state.db_backend,
    );
    Ok(crate::db::query(&sql)
        .bind(queue.job_id.as_str())
        .bind(&unit.collection)
        .bind(&unit.did)
        .execute(&state.backfill_db)
        .await?
        .rows_affected()
        > 0)
}

/// Give a fetched unit's window slot back after trying to complete it.
///
/// `removed` is what the write returned: `Some(false)` means there was no row
/// to remove, so no slot to free. A write that failed (`None`) leaves the row
/// behind, but the slot is freed anyway: the unit is resolved, so this run
/// will not pick it up again, and holding its slot for good could stall
/// discovery behind it. The window undercounts by one until the job restarts
/// and recounts; the job is paused rather than completed with the unit left.
fn release_slot(queue: &JobQueue, removed: Option<bool>) {
    if removed != Some(false) {
        queue.window.release(1);
    }
}

/// Count a fetch give-up against its PDS.
async fn count_pds_error(state: &AppState, queue: &JobQueue, pds: &str) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, errors) VALUES (?, ?, 1) \
         ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET errors = happyview_backfill_pds_stats.errors + 1",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(queue.job_id.as_str())
        .bind(pds)
        .execute(&state.backfill_db)
        .await
        .map(|_| ())
}

/// Count records a deferred retry fetched after its unit completed.
async fn add_late_records(
    state: &AppState,
    queue: &JobQueue,
    pds: &str,
    records: i32,
) -> Result<(), sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let mut tx = state.backfill_db.begin().await?;
    let counter = adapt_sql(
        "UPDATE happyview_backfill_jobs SET total_records = COALESCE(total_records, 0) + ? WHERE id = ?",
        backend,
    );
    crate::db::query(&counter)
        .bind(records)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    if queue.version == QueueVersion::Bounded {
        let stats = adapt_sql(
            "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, records) VALUES (?, ?, ?) \
             ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET records = happyview_backfill_pds_stats.records + excluded.records",
            backend,
        );
        crate::db::query(&stats)
            .bind(job_id)
            .bind(pds)
            .bind(i64::from(records))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

/// `(resolved, processed, records)` for a pipeline run to start from. A
/// bounded job's row holds exact counters; a legacy job's are recounted from
/// its repo rows, as before.
async fn seed_counters(state: &AppState, queue: &JobQueue) -> Result<(i32, i32, i32), sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let sql = adapt_sql(
        "SELECT resolved_repos, processed_repos, total_records FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let (resolved, processed, records): (Option<i32>, Option<i32>, Option<i32>) =
        crate::db::query_as(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await?;
    let records = records.unwrap_or(0);
    if queue.version == QueueVersion::Bounded {
        return Ok((resolved.unwrap_or(0), processed.unwrap_or(0), records));
    }

    let resolved_sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NOT NULL",
        backend,
    );
    let (resolved,): (i64,) = crate::db::query_as(&resolved_sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    let completed_sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND status = 'completed'",
        backend,
    );
    let (completed,): (i64,) = crate::db::query_as(&completed_sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    let resolved = i32::try_from(resolved).unwrap_or(i32::MAX);
    let completed = i32::try_from(completed).unwrap_or(i32::MAX);
    update_job_counter(state, job_id, "resolved_repos", resolved).await;
    update_job_counter(state, job_id, "processed_repos", completed).await;
    Ok((resolved, completed, records))
}

async fn queued_units(state: &AppState, job_id: &str) -> Result<i64, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_queue WHERE job_id = ?",
        state.db_backend,
    );
    Ok(crate::db::query_as::<(i64,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?
        .0)
}

/// Move a pre-upgrade job that had not finished discovering onto the bounded
/// queue. Its partial repo list is discarded in batches and discovery starts
/// over; a single-account job gets its one unit back. Nothing had been
/// fetched, so nothing is lost.
async fn convert_to_bounded(
    state: &AppState,
    job_id: &str,
    single_did: Option<&str>,
) -> Result<(), sqlx::Error> {
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id = ? AND did IN \
         (SELECT did FROM happyview_backfill_repos WHERE job_id = ? LIMIT ?)",
        state.db_backend,
    );
    loop {
        let deleted = crate::db::retry_on_busy(|| {
            crate::db::query(&delete_sql)
                .bind(job_id)
                .bind(job_id)
                .bind(LEGACY_DELETE_BATCH)
                .execute(&state.backfill_db)
        })
        .await?
        .rows_affected();
        if (deleted as i64) < LEGACY_DELETE_BATCH {
            break;
        }
    }

    crate::db::retry_on_busy(|| switch_to_bounded(state, job_id, single_did)).await?;

    if let Some(did) = single_did {
        publish_event(
            state,
            super::types::BackfillEvent::RepoDiscovered {
                job_id: job_id.to_string(),
                did: did.to_string(),
            },
        );
    }
    Ok(())
}

/// The job-row half of `convert_to_bounded`, with the single account's unit,
/// in one transaction.
async fn switch_to_bounded(
    state: &AppState,
    job_id: &str,
    single_did: Option<&str>,
) -> Result<(), sqlx::Error> {
    let backend = state.db_backend;
    let mut tx = state.backfill_db.begin().await?;
    let job_sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET queue_version = 2, discovery_complete = ?, total_repos = ?, \
         resolved_repos = 0, processed_repos = 0 WHERE id = ?",
        backend,
    );
    crate::db::query(&job_sql)
        .bind(i32::from(single_did.is_some()))
        .bind(i32::from(single_did.is_some()))
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    if let Some(did) = single_did {
        let unit_sql = adapt_sql(
            "INSERT INTO happyview_backfill_queue (job_id, collection, did) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
            backend,
        );
        crate::db::query(&unit_sql)
            .bind(job_id)
            .bind(ALL_COLLECTIONS)
            .bind(did)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// How far one collection's relay listing got.
#[derive(Clone, Debug)]
struct DiscoveryCursor {
    cursor: Option<String>,
    done: bool,
}

/// Why one collection's discovery stopped short.
enum DiscoveryError {
    /// The relay would not list the collection. As before, it is skipped.
    Relay(String),
    /// The queue could not be written. The job fails rather than finishing
    /// on a partial list.
    Database(String),
}

async fn load_discovery_cursors(
    state: &AppState,
    job_id: &str,
) -> Result<HashMap<String, DiscoveryCursor>, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT collection, relay_cursor, done FROM happyview_backfill_cursors WHERE job_id = ?",
        state.db_backend,
    );
    let rows: Vec<(String, Option<String>, i32)> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(collection, cursor, done)| {
            (
                collection,
                DiscoveryCursor {
                    cursor,
                    done: done != 0,
                },
            )
        })
        .collect())
}

async fn job_total_repos(state: &AppState, job_id: &str) -> Result<i32, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT total_repos FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let (total,): (Option<i32>,) = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    Ok(total.unwrap_or(0))
}

/// One `listReposByCollection` page, sleeping through rate limits. `Ok(None)`
/// means the job stopped while it waited out a rate limit.
async fn fetch_relay_page(
    state: &AppState,
    url: &str,
    collection: &str,
    cancelled: &AtomicBool,
) -> Result<Option<ListReposResponse>, String> {
    let resp = loop {
        let r = state
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| format!("relay request failed: {e}"))?;
        if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = parse_retry_after(r.headers());
            tracing::warn!(collection, wait, "rate limited by relay, sleeping");
            // In slices, so a pause or cancel is not held up by a long reset.
            let until = tokio::time::Instant::now() + Duration::from_secs(wait);
            while tokio::time::Instant::now() < until {
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(None);
                }
                tokio::time::sleep_until(
                    until.min(tokio::time::Instant::now() + Duration::from_secs(1)),
                )
                .await;
            }
            continue;
        }
        break r;
    };
    if !resp.status().is_success() {
        return Err(format!("relay returned {}", resp.status()));
    }
    resp.json()
        .await
        .map(Some)
        .map_err(|e| format!("invalid relay response: {e}"))
}

/// Enqueue one relay page, save the cursor that follows it and add to the
/// job's discovered count, in one transaction, so a restart resumes after the
/// last page enqueued. Returns the units added; a DID already queued for this
/// collection is not added twice.
async fn enqueue_discovered_page(
    state: &AppState,
    job_id: &str,
    collection: &str,
    dids: &[String],
    next_cursor: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let backend = state.db_backend;
    let mut tx = state.backfill_db.begin().await?;
    let mut added: i64 = 0;
    for chunk in dids.chunks(DISCOVERY_INSERT_CHUNK) {
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql = adapt_sql(
            &format!(
                "INSERT INTO happyview_backfill_queue (job_id, collection, did) VALUES {placeholders} ON CONFLICT DO NOTHING"
            ),
            backend,
        );
        let mut insert = crate::db::query(&sql);
        for did in chunk {
            insert = insert.bind(job_id).bind(collection).bind(did.as_str());
        }
        added += insert.execute(&mut *tx).await?.rows_affected() as i64;
    }
    let cursor_sql = adapt_sql(
        "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES (?, ?, ?, ?) \
         ON CONFLICT (job_id, collection) DO UPDATE SET relay_cursor = excluded.relay_cursor, done = excluded.done",
        backend,
    );
    crate::db::query(&cursor_sql)
        .bind(job_id)
        .bind(collection)
        .bind(next_cursor)
        .bind(i32::from(next_cursor.is_none()))
        .execute(&mut *tx)
        .await?;
    let total_sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET total_repos = COALESCE(total_repos, 0) + ? WHERE id = ?",
        backend,
    );
    crate::db::query(&total_sql)
        .bind(i32::try_from(added).unwrap_or(i32::MAX))
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(added)
}

/// Wait until the window has room for `n` units and reserve it. Returns false
/// if the job is stopping.
async fn wait_for_window(
    state: &AppState,
    queue: &JobQueue,
    n: i64,
    cancelled: &AtomicBool,
) -> bool {
    let mut polls: u32 = 0;
    while !queue.window.try_reserve(n) {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        // Checking the job row every couple of seconds is enough for a loop
        // that is only waiting.
        if polls.is_multiple_of(8) && should_stop_worker(state, &queue.job_id).await {
            return false;
        }
        polls = polls.wrapping_add(1);
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    true
}

/// Page one collection's repos into the queue, starting at `cursor`.
/// `Ok(true)` means the relay has no more pages; `Ok(false)` means the job is
/// stopping.
async fn discover_collection(
    state: &AppState,
    queue: &JobQueue,
    collection: &str,
    mut cursor: Option<String>,
    total: &AtomicI32,
    cancelled: &AtomicBool,
) -> Result<bool, DiscoveryError> {
    let job_id = queue.job_id.as_str();
    let base = state.config.relay_url.trim_end_matches('/');
    // A page must fit into an empty window, or a small window would never
    // admit one.
    let page_limit = queue.window.capacity().min(RELAY_PAGE_LIMIT);

    loop {
        if !wait_for_window(state, queue, page_limit, cancelled).await {
            return Ok(false);
        }

        let mut url = format!(
            "{base}/xrpc/com.atproto.sync.listReposByCollection?collection={collection}&limit={page_limit}"
        );
        if let Some(ref c) = cursor {
            url.push_str(&format!("&cursor={c}"));
        }
        let body = match fetch_relay_page(state, &url, collection, cancelled).await {
            Ok(Some(body)) => body,
            Ok(None) => {
                queue.window.settle(page_limit, 0);
                return Ok(false);
            }
            Err(e) => {
                queue.window.settle(page_limit, 0);
                return Err(DiscoveryError::Relay(e));
            }
        };

        let page_count = body.repos.len();
        let next = match body.cursor {
            Some(c) if page_count > 0 => Some(c),
            _ => None,
        };
        let dids: Vec<String> = body.repos.into_iter().map(|repo| repo.did).collect();
        let enqueued = crate::db::retry_on_busy(|| {
            enqueue_discovered_page(state, job_id, collection, &dids, next.as_deref())
        })
        .await;
        let added = match enqueued {
            Ok(added) => added,
            Err(e) => {
                queue.window.settle(page_limit, 0);
                return Err(DiscoveryError::Database(format!(
                    "failed to enqueue repos discovered for {collection}: {e}"
                )));
            }
        };
        queue.window.settle(page_limit, added);

        let added = i32::try_from(added).unwrap_or(i32::MAX);
        let running = total.fetch_add(added, Ordering::Relaxed) + added;
        publish_event(
            state,
            super::types::BackfillEvent::JobCounters {
                job_id: job_id.to_string(),
                total_repos: Some(running),
                resolved_repos: None,
                processed_repos: None,
                total_records: None,
            },
        );

        if next.is_none() {
            return Ok(true);
        }
        if cancelled.load(Ordering::Relaxed) || should_stop_worker(state, job_id).await {
            return Ok(false);
        }
        cursor = next;
    }
}

/// Discover a network job's repos through the relay while the resolver and
/// fetchers work through them. At most the window's worth is queued at once:
/// a full window pauses discovery until units complete.
///
/// An `Err` means discovery could not read or write the queue, and the job
/// must fail: it sets `cancelled` so the resolver and fetchers stop too,
/// rather than draining what was queued and leaving the job looking complete.
async fn run_discovery(
    state: AppState,
    queue: JobQueue,
    collections: Vec<String>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    let _done = MarkDiscoveryDone(Arc::clone(&queue.discovery_done));
    let result = discover_all(&state, &queue, collections, &cancelled).await;
    if result.is_err() {
        cancelled.store(true, Ordering::Relaxed);
    }
    result
}

async fn discover_all(
    state: &AppState,
    queue: &JobQueue,
    collections: Vec<String>,
    cancelled: &Arc<AtomicBool>,
) -> Result<(), String> {
    let job_id = queue.job_id.as_str();
    let cursors = load_discovery_cursors(state, job_id)
        .await
        .map_err(|e| format!("failed to read the backfill discovery cursors: {e}"))?;
    let total = Arc::new(AtomicI32::new(
        job_total_repos(state, job_id)
            .await
            .map_err(|e| format!("failed to read the backfill job's discovered count: {e}"))?,
    ));

    // Each future owns its handles rather than borrowing this function's, so
    // the stream stays `Send` inside the spawned discovery task.
    let outcomes: Vec<(String, Result<bool, DiscoveryError>)> = stream::iter(collections)
        .map(|collection| {
            let start = cursors.get(&collection).cloned();
            let state = state.clone();
            let queue = queue.clone();
            let total = Arc::clone(&total);
            let cancelled = Arc::clone(cancelled);
            async move {
                let outcome = match start {
                    Some(DiscoveryCursor { done: true, .. }) => Ok(true),
                    other => {
                        discover_collection(
                            &state,
                            &queue,
                            &collection,
                            other.and_then(|c| c.cursor),
                            &total,
                            &cancelled,
                        )
                        .await
                    }
                };
                (collection, outcome)
            }
        })
        .buffer_unordered(5)
        .collect()
        .await;

    let mut stopped = false;
    let mut database_error = None;
    for (collection, outcome) in outcomes {
        match outcome {
            Ok(true) => {}
            Ok(false) => stopped = true,
            Err(DiscoveryError::Database(e)) => {
                tracing::error!(job_id, collection = %collection, error = %e, "backfill discovery could not write its queue");
                database_error = Some(e);
            }
            Err(DiscoveryError::Relay(e)) => {
                // As before, a collection the relay will not list is skipped
                // rather than failing the job.
                tracing::warn!(job_id, collection = %collection, error = %e, "failed to discover repos, skipping");
                log_event(
                    &state.db,
                    EventLog {
                        event_type: "backfill.discovery_failed".to_string(),
                        severity: Severity::Warn,
                        actor_did: None,
                        subject: Some(collection.clone()),
                        detail: serde_json::json!({
                            "job_id": job_id,
                            "collection": collection,
                            "error": e,
                        }),
                    },
                    state.db_backend,
                )
                .await;
            }
        }
    }
    if let Some(e) = database_error {
        return Err(e);
    }
    if stopped {
        return Ok(());
    }

    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET discovery_complete = 1 WHERE id = ?",
        state.db_backend,
    );
    job_write(state, job_id, "discovery_complete", || {
        crate::db::query(&sql)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await
    .ok_or_else(|| "failed to record that backfill discovery finished".to_string())?;
    if let Some(snapshot) = build_job_snapshot(state, job_id).await {
        publish_event(state, snapshot);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pipelined resolve and fetch
// ---------------------------------------------------------------------------

/// What the resolver task owns.
struct ResolverContext {
    state: AppState,
    queue: JobQueue,
    resolved: Arc<AtomicI32>,
    cancelled: Arc<AtomicBool>,
    recorder: Arc<super::backfill_errors::ErrorRecorder>,
    concurrency: usize,
    tx: mpsc::Sender<(WorkUnit, String)>,
}

/// The resolver's retry bookkeeping. It is local to the one resolver task and
/// keyed to the hosts that task talks to.
struct ResolverRun {
    cooldowns: HostCooldowns,
    deferred: DeferredQueue<WorkUnit>,
    /// Units waiting in `deferred`. They are still unresolved rows, so a later
    /// pass over the queue would otherwise resolve them a second time.
    deferred_units: HashSet<WorkUnit>,
    /// DIDs with a resolve give-up recorded. A DID found under several
    /// collections fails once per unit, and the detail table is keyed per
    /// DID, so the DID is recorded once.
    recorded: HashSet<String>,
    /// PDS endpoints resolved this run, so a DID queued under several
    /// collections is looked up once rather than once per unit.
    resolved_dids: ResolvedDids,
    max_attempts: u32,
    attempted: i32,
    next_cancel_check: i32,
}

/// DIDs a resolver has looked up this run, most recent `RESOLVED_DID_CACHE`.
#[derive(Default)]
struct ResolvedDids {
    pds: HashMap<String, String>,
    order: VecDeque<String>,
}

/// DIDs the resolver remembers. A DID's units are queued close together (one
/// per collection, each page of each collection's relay listing at a time),
/// so a small cache catches nearly every repeat.
const RESOLVED_DID_CACHE: usize = 10_000;

impl ResolvedDids {
    fn get(&self, did: &str) -> Option<String> {
        self.pds.get(did).cloned()
    }

    /// Remember `did`'s PDS, forgetting the oldest entry once full.
    fn insert(&mut self, did: &str, pds: &str) {
        if self.pds.insert(did.to_string(), pds.to_string()).is_some() {
            return;
        }
        self.order.push_back(did.to_string());
        if self.order.len() > RESOLVED_DID_CACHE
            && let Some(oldest) = self.order.pop_front()
        {
            self.pds.remove(&oldest);
        }
    }
}

/// Hand the fetcher every unit an earlier run resolved but never fetched, a
/// page at a time. This finishes before this run resolves anything, so every
/// resolved unit it reads came from an earlier run and none is sent twice.
/// Returns false if the job is stopping or the fetcher has gone away.
async fn send_resolved_backlog(ctx: &ResolverContext) -> Result<bool, sqlx::Error> {
    let mut after: Option<WorkUnit> = None;
    loop {
        let page = resolved_page(&ctx.state, &ctx.queue, after.as_ref()).await?;
        let Some((last, _)) = page.last().cloned() else {
            return Ok(true);
        };
        after = Some(last);
        for pair in page {
            if ctx.cancelled.load(Ordering::Relaxed) || ctx.tx.send(pair).await.is_err() {
                return Ok(false);
            }
        }
    }
}

/// Resolve every unresolved unit, a page at a time.
///
/// A legacy job's list is complete before this starts, so one pass covers it.
/// A bounded job's list grows while discovery runs, and discovery can enqueue
/// a unit behind the cursor. Passes therefore repeat until one that *began*
/// after discovery finished comes up empty. Between passes, retries that have
/// come due run, because deferred units hold window slots discovery may be
/// waiting on.
///
/// An `Err` means the queue could not be read; `cancelled` is set so the
/// fetchers stop, and the job fails.
async fn run_resolver(ctx: ResolverContext) -> Result<(), String> {
    match send_resolved_backlog(&ctx).await {
        Ok(true) => {}
        Ok(false) => return Ok(()),
        Err(e) => {
            ctx.cancelled.store(true, Ordering::Relaxed);
            return Err(format!("failed to read resolved backfill units: {e}"));
        }
    }

    let mut run = ResolverRun {
        cooldowns: HostCooldowns::new(),
        deferred: DeferredQueue::new(),
        deferred_units: HashSet::new(),
        recorded: HashSet::new(),
        resolved_dids: ResolvedDids::default(),
        max_attempts: load_max_attempts(&ctx.state).await,
        attempted: 0,
        next_cancel_check: random_batch_threshold(10),
    };
    let mut after: Option<WorkUnit> = None;
    let mut pass_began_after_discovery = ctx.queue.discovery_done.load(Ordering::Acquire);

    'scan: loop {
        if ctx.cancelled.load(Ordering::Relaxed) {
            break;
        }
        let page = match unresolved_page(&ctx.state, &ctx.queue, after.as_ref()).await {
            Ok(page) => page,
            Err(e) => {
                ctx.cancelled.store(true, Ordering::Relaxed);
                ctx.recorder.flush(&ctx.state, &ctx.queue.job_id).await;
                return Err(format!("failed to read unresolved backfill units: {e}"));
            }
        };
        let Some(last) = page.last().cloned() else {
            if pass_began_after_discovery {
                break;
            }
            if !ctx.queue.discovery_done.load(Ordering::Acquire)
                && !idle_drain(&ctx, &mut run).await
            {
                break;
            }
            after = None;
            pass_began_after_discovery = ctx.queue.discovery_done.load(Ordering::Acquire);
            continue;
        };
        after = Some(last);

        // Units of a DID this run already resolved need no lookup, and the
        // rest are looked up once per DID however many collections queued it.
        let mut known: Vec<(WorkUnit, String)> = Vec::new();
        let mut lookups: Vec<(String, Vec<WorkUnit>)> = Vec::new();
        let mut lookup_index: HashMap<String, usize> = HashMap::new();
        for unit in page {
            if run.deferred_units.contains(&unit) {
                continue;
            }
            if let Some(pds) = run.resolved_dids.get(&unit.did) {
                known.push((unit, pds));
                continue;
            }
            match lookup_index.get(&unit.did) {
                Some(&i) => lookups[i].1.push(unit),
                None => {
                    lookup_index.insert(unit.did.clone(), lookups.len());
                    lookups.push((unit.did.clone(), vec![unit]));
                }
            }
        }
        for (unit, pds) in known {
            if !on_cached(&ctx, &mut run, unit, pds).await {
                warn_fetcher_gone(&ctx, &run);
                break 'scan;
            }
        }

        let stream_state = ctx.state.clone();
        let stream_cancelled = Arc::clone(&ctx.cancelled);
        let mut results = stream::iter(lookups)
            .map(move |(did, units)| {
                let state = stream_state.clone();
                let cancelled = Arc::clone(&stream_cancelled);
                async move {
                    if cancelled.load(Ordering::Relaxed) {
                        return None;
                    }
                    let result = profile::resolve_pds_endpoint_once(
                        &state.http,
                        &state.config.plc_url,
                        &did,
                    )
                    .await;
                    Some((units, result))
                }
            })
            .buffer_unordered(ctx.concurrency);

        while let Some(item) = results.next().await {
            let Some((units, result)) = item else {
                break 'scan;
            };
            for unit in units {
                if !handle_resolution(&ctx, &mut run, unit, result.clone(), 1).await {
                    warn_fetcher_gone(&ctx, &run);
                    break 'scan;
                }
            }
            run.attempted += 1;
            if run.attempted >= run.next_cancel_check {
                if should_stop_worker(&ctx.state, &ctx.queue.job_id).await {
                    ctx.cancelled.store(true, Ordering::Relaxed);
                    break 'scan;
                }
                run.next_cancel_check = run.attempted + random_batch_threshold(10);
            }
        }
    }

    // Final deferred pass. Nothing new is coming, so whatever is left is
    // waiting on a clock rather than on work.
    loop {
        if ctx.cancelled.load(Ordering::Relaxed) {
            break;
        }
        match next_drain_step(&mut run.deferred, &run.cooldowns, Duration::from_secs(2)).await {
            DrainStep::Done => break,
            DrainStep::Slept => {
                if should_stop_worker(&ctx.state, &ctx.queue.job_id).await {
                    ctx.cancelled.store(true, Ordering::Relaxed);
                    break;
                }
            }
            DrainStep::Retry(item) => {
                if !retry_deferred_resolution(&ctx, &mut run, item).await {
                    break;
                }
            }
        }
    }

    ctx.recorder.flush(&ctx.state, &ctx.queue.job_id).await;
    // `ctx.tx` drops here, which is how the fetcher learns no more units are
    // coming. That is why the deferred pass has to finish first.
    Ok(())
}

fn warn_fetcher_gone(ctx: &ResolverContext, run: &ResolverRun) {
    tracing::warn!(
        job_id = %ctx.queue.job_id,
        deferred_queued = run.deferred.len(),
        "fetcher channel closed while resolving; abandoning the remaining units — \
         they will not be fetched, counted, or recorded as errors"
    );
}

/// Between passes, while discovery is still enqueueing, retry the deferred
/// resolutions that are due and then wait briefly. Returns false if the job
/// is stopping or the fetcher has gone away.
async fn idle_drain(ctx: &ResolverContext, run: &mut ResolverRun) -> bool {
    loop {
        if ctx.cancelled.load(Ordering::Relaxed) {
            return false;
        }
        match next_drain_step(
            &mut run.deferred,
            &run.cooldowns,
            Duration::from_millis(500),
        )
        .await
        {
            DrainStep::Retry(item) => {
                if !retry_deferred_resolution(ctx, run, item).await {
                    return false;
                }
            }
            DrainStep::Slept => break,
            DrainStep::Done => {
                tokio::time::sleep(Duration::from_millis(250)).await;
                break;
            }
        }
    }
    if should_stop_worker(&ctx.state, &ctx.queue.job_id).await {
        ctx.cancelled.store(true, Ordering::Relaxed);
        return false;
    }
    true
}

/// Act on one resolution attempt. Returns false once the fetcher has gone away.
async fn handle_resolution(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    unit: WorkUnit,
    result: Result<String, crate::admin::backfill_errors::BackfillFailure>,
    attempts: u32,
) -> bool {
    let host = profile::did_doc_host(&ctx.state.config.plc_url, &unit.did);
    let failure = match result {
        Ok(pds) => {
            run.cooldowns.record_success(&host);
            run.deferred_units.remove(&unit);
            run.resolved_dids.insert(&unit.did, &pds);
            return on_resolved(ctx, unit, pds).await;
        }
        Err(failure) => failure,
    };

    let retry = failure.kind.is_retryable() && attempts < run.max_attempts;
    if !retry {
        tracing::warn!(
            did = %unit.did,
            kind = failure.kind.as_str(),
            attempts,
            "giving up resolving PDS endpoint: {}",
            failure.message
        );
        give_up_resolution(ctx, run, unit, &failure, attempts).await;
        return true;
    }

    let now = std::time::Instant::now();
    run.cooldowns
        .record_failure(&host, failure.retry_after, now);
    run.deferred_units.insert(unit.clone());
    run.deferred.push(DeferredItem {
        payload: unit,
        host: host.clone(),
        attempts,
        eligible_at: now,
    });
    // A host that has stopped answering must not be asked again once per
    // cooldown for every DID behind it. Declare it down and record its whole
    // queue at once. This only happens on a retry, as before: one
    // first-attempt failure says little.
    if attempts > 1 && run.cooldowns.is_saturated(&host) {
        let abandoned = run.deferred.drain_host(&host);
        tracing::warn!(
            host = %host,
            abandoned = abandoned.len(),
            kind = failure.kind.as_str(),
            "host failed {} times consecutively; giving up on its remaining deferred resolutions: {}",
            run.cooldowns.consecutive_failures(&host),
            failure.message
        );
        for item in abandoned {
            give_up_resolution(ctx, run, item.payload, &failure, item.attempts).await;
        }
    }
    true
}

async fn retry_deferred_resolution(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    item: DeferredItem<WorkUnit>,
) -> bool {
    let attempts = item.attempts + 1;
    // Another unit of the same DID may have resolved since this one was
    // deferred.
    let handed_on = match run.resolved_dids.get(&item.payload.did) {
        Some(pds) => on_cached(ctx, run, item.payload, pds).await,
        None => {
            let result = profile::resolve_pds_endpoint_once(
                &ctx.state.http,
                &ctx.state.config.plc_url,
                &item.payload.did,
            )
            .await;
            handle_resolution(ctx, run, item.payload, result, attempts).await
        }
    };
    if handed_on {
        return true;
    }
    tracing::warn!(
        job_id = %ctx.queue.job_id,
        deferred_queued = run.deferred.len(),
        "fetcher channel closed while draining deferred resolutions; abandoning the remaining \
         queued units — they will not be fetched, counted, or recorded as errors"
    );
    false
}

/// Resolve a unit from the DID cache. No request was sent, so the host's
/// cooldown is left exactly as it was: for `did:plc` the host is the shared
/// PLC directory, and treating a cache hit as a success there would clear a
/// rate limit it never lifted. Returns false once the fetcher has gone away.
async fn on_cached(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    unit: WorkUnit,
    pds: String,
) -> bool {
    run.deferred_units.remove(&unit);
    on_resolved(ctx, unit, pds).await
}

/// Record a resolve give-up (once per DID) and take the unit out of the queue,
/// freeing its window slot.
async fn give_up_resolution(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    unit: WorkUnit,
    failure: &crate::admin::backfill_errors::BackfillFailure,
    attempts: u32,
) {
    run.deferred_units.remove(&unit);
    if run.recorded.insert(unit.did.clone()) {
        ctx.recorder
            .record(
                &ctx.state,
                &ctx.queue.job_id,
                &unit.did,
                None,
                "resolve",
                failure,
                attempts,
            )
            .await;
    }
    let removed = job_write(&ctx.state, &ctx.queue.job_id, "drop_unresolvable", || {
        drop_unresolvable(&ctx.state, &ctx.queue, &unit)
    })
    .await;
    // Only a removed row frees a slot. A drop that failed leaves the unit
    // unresolved, so a later pass gives it up again and frees it then;
    // freeing it now too would count the slot twice.
    if removed == Some(true) {
        ctx.queue.window.release(1);
    }
}

/// Record a resolved unit and hand it to the fetcher. Returns false once the
/// fetcher has gone away.
///
/// A unit whose PDS could not be stored is not fetched: it stays unresolved
/// in the queue, so a later pass (or the resumed job) resolves it again
/// instead of fetching a unit the queue does not know is in flight.
async fn on_resolved(ctx: &ResolverContext, unit: WorkUnit, pds: String) -> bool {
    let stored = job_write(&ctx.state, &ctx.queue.job_id, "mark_resolved", || {
        mark_resolved(&ctx.state, &ctx.queue, &unit, &pds)
    })
    .await;
    if stored.is_none() {
        return true;
    }
    publish_event(
        &ctx.state,
        super::types::BackfillEvent::RepoResolved {
            job_id: ctx.queue.job_id.to_string(),
            did: unit.did.clone(),
            pds_endpoint: pds.clone(),
        },
    );
    let count = ctx.resolved.fetch_add(1, Ordering::Relaxed) + 1;
    publish_event(
        &ctx.state,
        super::types::BackfillEvent::JobCounters {
            job_id: ctx.queue.job_id.to_string(),
            total_repos: None,
            resolved_repos: Some(count),
            processed_repos: None,
            total_records: None,
        },
    );
    ctx.tx.send((unit, pds)).await.is_ok()
}

/// Resolve and fetch a job's units until none remain, returning the job's
/// `(processed_repos, total_records)`. An `Err` means the queue could not be
/// read and the job must fail.
///
/// `cancelled` is shared with discovery, which sets it when it fails.
async fn run_pipelined_resolve_and_fetch(
    state: &AppState,
    collections: &[String],
    concurrency: &BackfillConcurrency,
    queue: &JobQueue,
    cancelled: &Arc<AtomicBool>,
) -> Result<(i32, i32), String> {
    let job_id = queue.job_id.as_str();
    set_stage(state, job_id, "resolving_and_fetching").await;

    let (already_resolved, already_completed, existing_records) = seed_counters(state, queue)
        .await
        .map_err(|e| format!("failed to read backfill progress counters: {e}"))?;
    let resolved_repos = Arc::new(AtomicI32::new(already_resolved));
    let processed_repos = Arc::new(AtomicI32::new(already_completed));
    let total_records = Arc::new(AtomicI32::new(existing_records));

    let (tx, mut rx) = mpsc::channel::<(WorkUnit, String)>(256);

    // One error sink for the whole job, shared by the resolver and every PDS
    // worker — see `ErrorRecorder`'s doc comment for why it must not be
    // constructed per worker.
    let recorder = Arc::new(super::backfill_errors::ErrorRecorder::new(state, job_id).await);

    // The resolver first hands over units an earlier run resolved but never
    // fetched, then resolves the rest. It holds the only sender, so the
    // channel closes when it finishes.
    let resolver_handle = tokio::spawn(run_resolver(ResolverContext {
        state: state.clone(),
        queue: queue.clone(),
        resolved: Arc::clone(&resolved_repos),
        cancelled: Arc::clone(cancelled),
        recorder: Arc::clone(&recorder),
        concurrency: concurrency.resolution,
        tx,
    }));

    // --- Fetcher: receive (unit, pds) pairs and dispatch to PDS workers ---
    // Each PDS gets its own worker with a unit channel, and every worker starts
    // immediately — see `FetchContext::requests` for why gating startup on a
    // semaphore deadlocks the job. Concurrency is capped on in-flight requests
    // instead. We never hold the workers lock across an `.await` — use
    // `try_send` to avoid blocking when a worker's channel is full (overflow
    // goes to a retry queue drained on each iteration).
    let state = Arc::new(state.clone());

    // Derived from the two existing settings rather than introduced as a third,
    // so every deployment keeps the effective concurrency it has today: the old
    // scheme allowed `pds` workers each with `dids_per_pds` fetches in flight.
    // The difference is that those requests are no longer confined to `pds`
    // endpoints — they spread across every PDS in the job, which is what stops
    // a handful of hosts absorbing the whole rate-limit budget while the rest
    // sit idle.
    let request_limit = concurrency
        .pds
        .saturating_mul(concurrency.dids_per_pds)
        .max(1);
    let worker_ctx = FetchContext {
        state: Arc::clone(&state),
        queue: queue.clone(),
        collections: Arc::new(collections.to_vec()),
        processed_repos: Arc::clone(&processed_repos),
        total_records: Arc::clone(&total_records),
        cancelled: Arc::clone(cancelled),
        dids_per_pds: concurrency.dids_per_pds,
        recorder: Arc::clone(&recorder),
        requests: Arc::new(tokio::sync::Semaphore::new(request_limit)),
    };
    let mut pds_workers: HashMap<String, mpsc::Sender<WorkUnit>> = HashMap::new();
    let mut worker_handles = FuturesUnordered::new();
    // Units a PDS worker had no room for yet, per PDS, in arrival order.
    let mut waiting: HashMap<String, VecDeque<WorkUnit>> = HashMap::new();
    let mut rx_open = true;
    // Retries the waiting units even when nothing new arrives: once the
    // window is full, the resolver sends nothing until units complete, and
    // units only complete once the workers get them.
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_stop_check = std::time::Instant::now();

    loop {
        if cancelled.load(Ordering::Relaxed) || (!rx_open && waiting.is_empty()) {
            break;
        }
        tokio::select! {
            received = rx.recv(), if rx_open => match received {
                Some((unit, pds_endpoint)) => {
                    waiting.entry(pds_endpoint).or_default().push_back(unit);
                }
                None => rx_open = false,
            },
            _ = tick.tick() => {
                if last_stop_check.elapsed() >= Duration::from_millis(500) {
                    last_stop_check = std::time::Instant::now();
                    if should_stop_worker(&state, &queue.job_id).await {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        dispatch_waiting(
            &worker_ctx,
            &mut pds_workers,
            &mut worker_handles,
            &mut waiting,
        );

        // Drain any completed worker handles to avoid unbounded accumulation.
        // An empty set is ready with `None`, which ends the drain rather than
        // spinning on it.
        while let Some(Some(result)) = worker_handles.next().now_or_never() {
            if let Err(e) = result {
                tracing::warn!(error = %e, "PDS worker task panicked");
            }
        }
    }

    // Drop all PDS senders so workers know no more units are coming.
    drop(pds_workers);
    while let Some(result) = worker_handles.next().await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "PDS worker task panicked");
        }
    }
    let resolved = match resolver_handle.await {
        Ok(result) => result,
        Err(e) => Err(format!("backfill resolver task panicked: {e}")),
    };

    // Flush again now that every PDS worker (and the resolver) has finished.
    // The resolver already flushed once inside its own task when resolution
    // finished, but that predates most of the fetch phase's give-ups —
    // fetching is the long pole, so flushing only there left `error_counts`
    // frozen at a resolve-only snapshot. This must come after the joins
    // above: flushing earlier could race the resolver's own flush and get
    // overwritten by its older counts.
    recorder.flush(&state, job_id).await;
    if queue.version == QueueVersion::Bounded {
        job_write(&state, job_id, "trim_completions", || {
            trim_completions(&state, job_id)
        })
        .await;
    }
    resolved?;

    // The job row's counters were kept current unit by unit; these are the
    // same numbers for the caller's `complete_job`.
    Ok((
        processed_repos.load(Ordering::Relaxed),
        total_records.load(Ordering::Relaxed),
    ))
}

/// Hand each PDS's waiting units to its worker until the worker's channel is
/// full, starting a worker for a PDS that has none (or whose worker exited).
/// Only PDSes with units waiting are visited, so a long backlog for one host
/// costs nothing per unit received for another.
///
/// Never blocks: every worker starts consuming the moment it exists (see
/// `FetchContext::requests`), and a full channel just leaves the rest waiting
/// for the next call.
fn dispatch_waiting(
    template: &FetchContext,
    workers: &mut HashMap<String, mpsc::Sender<WorkUnit>>,
    handles: &mut FuturesUnordered<tokio::task::JoinHandle<()>>,
    waiting: &mut HashMap<String, VecDeque<WorkUnit>>,
) {
    for (pds_endpoint, units) in waiting.iter_mut() {
        while let Some(unit) = units.pop_front() {
            let Some(pds_tx) = workers.get(pds_endpoint) else {
                spawn_pds_worker(template, workers, handles, pds_endpoint.clone(), unit);
                continue;
            };
            match pds_tx.try_send(unit) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(unit)) => {
                    units.push_front(unit);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(unit)) => {
                    // The worker finished; replace it.
                    workers.remove(pds_endpoint);
                    spawn_pds_worker(template, workers, handles, pds_endpoint.clone(), unit);
                }
            }
        }
    }
    waiting.retain(|_, units| !units.is_empty());
}

/// Start a PDS worker with `first` already queued, and register its sender.
fn spawn_pds_worker(
    template: &FetchContext,
    workers: &mut HashMap<String, mpsc::Sender<WorkUnit>>,
    handles: &mut FuturesUnordered<tokio::task::JoinHandle<()>>,
    pds_endpoint: String,
    first: WorkUnit,
) {
    let (pds_tx, pds_rx) = mpsc::channel::<WorkUnit>(64);
    pds_tx
        .try_send(first)
        .expect("a new channel has room for its first unit");
    workers.insert(pds_endpoint.clone(), pds_tx);
    let ctx = template.clone();
    handles.push(tokio::spawn(async move {
        run_pds_worker(ctx, pds_endpoint, pds_rx).await;
    }));
}

#[derive(Clone)]
struct FetchContext {
    state: Arc<AppState>,
    queue: JobQueue,
    collections: Arc<Vec<String>>,
    processed_repos: Arc<AtomicI32>,
    total_records: Arc<AtomicI32>,
    cancelled: Arc<AtomicBool>,
    dids_per_pds: usize,
    // Shared with every other PDS worker for this job — see the doc comment
    // on `recorder`'s construction in `run_pipelined_resolve_and_fetch` for
    // why this must be a clone, never a fresh `ErrorRecorder::new`.
    recorder: Arc<super::backfill_errors::ErrorRecorder>,
    /// Caps in-flight PDS requests across the whole job.
    ///
    /// This gates *requests*, never worker startup. Gating startup deadlocks:
    /// a worker cannot exit until its channel closes, and its channel closes
    /// only when the dispatcher finishes, so no permit is ever released during
    /// dispatch — a worker that never got one never drains its bounded queue,
    /// and the dispatcher then blocks forever trying to fill it. Every worker
    /// must be able to consume the moment its sender exists.
    requests: Arc<tokio::sync::Semaphore>,
}

/// DIDs already recorded as a fetch-phase give-up, so a DID that fails on
/// several collections produces one error row and one count, not N of each.
///
/// The detail table's primary key is `(job_id, did, phase)`, so the Nth write
/// for one DID upserts over the first while `ErrorCounts` gains N — which is
/// how a 20-lexicon job could report "20,000 failed" for 1,000 dead repos and
/// announce a cap it had not reached. Collapsing here rather than widening the
/// key keeps `backfill_errors_list`'s `did > ?` keyset cursor sound.
///
/// One set per worker is one set per job for any given DID, since a DID is
/// routed to exactly one PDS worker.
type RecordedDids = std::collections::HashSet<String>;

/// Order a DID's failed collections so a retryable failure is handled first.
///
/// Only the first give-up for a DID is recorded, and a retryable kind is the
/// more useful one to keep: `retry-failed` selects on retryability, so this is
/// what keeps the DID reachable by a retry job.
fn retryable_give_ups_first(failures: &mut [(String, FetchOutcome)]) {
    failures.sort_by_key(|(_, outcome)| match outcome {
        FetchOutcome::Failed { failure, .. } => !failure.kind.is_retryable(),
        FetchOutcome::Complete { .. } => true,
    });
}

/// One fetch failure for one (did, collection): either defer it for retry or
/// hand it to the recorder as a give-up.
///
/// Shared by every place a `FetchOutcome::Failed` is handled — the primary
/// per-DID results arm, the post-cancellation drain, and the deferred-retry
/// loop — so the retry/give-up policy can't drift between them.
#[allow(clippy::too_many_arguments)]
async fn defer_or_give_up_fetch(
    state: &AppState,
    queue: &JobQueue,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    cooldowns: &mut HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    max_attempts: u32,
    did: String,
    collection: String,
    cursor: Option<String>,
    failure: crate::admin::backfill_errors::BackfillFailure,
    attempts: u32,
) {
    let now = std::time::Instant::now();
    if failure.kind.is_retryable() && attempts < max_attempts {
        cooldowns.record_failure(pds_host, failure.retry_after, now);
        deferred.push(DeferredItem {
            payload: (did, collection, cursor),
            host: pds_host.to_string(),
            attempts,
            eligible_at: now,
        });
    } else {
        record_fetch_give_up(
            state,
            queue,
            pds_endpoint,
            recorder,
            recorded,
            &did,
            &collection,
            &failure,
            attempts,
        )
        .await;
        tracing::warn!(
            did,
            collection,
            pds = %pds_endpoint,
            kind = failure.kind.as_str(),
            attempts,
            "giving up fetching records from PDS: {}",
            failure.message
        );
    }
}

/// Record one fetch-phase give-up, at most once per DID per job.
///
/// The `tracing` line stays per-collection at every call site — an operator
/// reading logs wants to know which collection failed. Only the *row* and the
/// *count*, which are per-DID by the detail table's key, are collapsed.
#[allow(clippy::too_many_arguments)]
async fn record_fetch_give_up(
    state: &AppState,
    queue: &JobQueue,
    pds_endpoint: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    recorded: &mut RecordedDids,
    did: &str,
    collection: &str,
    failure: &crate::admin::backfill_errors::BackfillFailure,
    attempts: u32,
) {
    if !recorded.insert(did.to_string()) {
        return;
    }
    recorder
        .record(
            state,
            &queue.job_id,
            did,
            Some(collection),
            "fetch",
            failure,
            attempts,
        )
        .await;
    if queue.version == QueueVersion::Bounded {
        job_write(state, &queue.job_id, "pds_error", || {
            count_pds_error(state, queue, pds_endpoint)
        })
        .await;
    }
}

/// Give up on a PDS that has stopped answering, rather than re-offering it one
/// deferred item per cooldown.
///
/// A no-op unless the host is saturated, so the healthy path — a transient
/// rate limit that clears on the first successful retry — is untouched. Every
/// abandoned item still reaches the recorder carrying the failure that killed
/// the host, so the error taxonomy the dashboard reads is unchanged; only the
/// time taken to reach it stops scaling with the queue length.
#[allow(clippy::too_many_arguments)]
async fn abandon_saturated_pds(
    state: &AppState,
    queue: &JobQueue,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &super::backfill_errors::ErrorRecorder,
    cooldowns: &HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    failure: &crate::admin::backfill_errors::BackfillFailure,
) {
    // Only a host-level failure may be attributed to the rest of the queue.
    // A `repo_not_found` is a property of the one repo that provoked it, so
    // stamping it onto every other DID behind this host would misreport them
    // in exactly the way this feature exists to prevent — and a definitive
    // answer from a live server is evidence the host is answering, not that
    // it is down.
    if !failure.kind.is_retryable() || !cooldowns.is_saturated(pds_host) {
        return;
    }
    let abandoned = deferred.drain_host(pds_host);
    if abandoned.is_empty() {
        return;
    }
    tracing::warn!(
        pds = %pds_endpoint,
        host = pds_host,
        abandoned = abandoned.len(),
        kind = failure.kind.as_str(),
        "PDS failed {} times consecutively; giving up on its remaining deferred \
         fetches: {}",
        cooldowns.consecutive_failures(pds_host),
        failure.message
    );
    for item in abandoned {
        let (did, collection, _cursor) = item.payload;
        record_fetch_give_up(
            state,
            queue,
            pds_endpoint,
            recorder,
            recorded,
            &did,
            &collection,
            failure,
            item.attempts,
        )
        .await;
    }
}

/// The result of one unit's first attempt: records fetched, whether any
/// collection succeeded (clears the host's cooldown), and the per-collection
/// failures still needing a defer-or-give-up decision.
type UnitFetchResult = (WorkUnit, i32, bool, Vec<(String, FetchOutcome)>);

/// What settling a unit needs from its worker.
struct WorkerScope<'a> {
    state: &'a AppState,
    queue: &'a JobQueue,
    pds_endpoint: &'a str,
    pds_host: &'a str,
    recorder: &'a super::backfill_errors::ErrorRecorder,
    max_attempts: u32,
    processed_repos: &'a AtomicI32,
    total_records: &'a AtomicI32,
    cancelled: &'a AtomicBool,
}

/// A worker's retry bookkeeping. It is local to the worker and keyed to the
/// one host that worker talks to.
struct WorkerRetries {
    cooldowns: HostCooldowns,
    deferred: DeferredQueue<(String, String, Option<String>)>,
    recorded: RecordedDids,
}

/// Settle a unit's first attempt: defer or give up each collection that
/// failed, then complete the unit in one transaction. A unit cut short by a
/// pause or cancel stays queued so the resumed job fetches it again. Returns
/// the job's processed and record counts, or `None` if the unit was left
/// queued.
async fn finish_unit(
    scope: &WorkerScope<'_>,
    retries: &mut WorkerRetries,
    result: UnitFetchResult,
) -> Option<(i32, i32)> {
    let (unit, records, any_success, mut failures) = result;
    let records_now = scope.total_records.fetch_add(records, Ordering::Relaxed) + records;
    if scope.cancelled.load(Ordering::Relaxed) {
        return None;
    }
    if any_success {
        retries.cooldowns.record_success(scope.pds_host);
    }
    retryable_give_ups_first(&mut failures);
    for (collection, outcome) in failures {
        let FetchOutcome::Failed {
            cursor, failure, ..
        } = outcome
        else {
            continue;
        };
        defer_or_give_up_fetch(
            scope.state,
            scope.queue,
            scope.pds_endpoint,
            scope.pds_host,
            scope.recorder,
            &mut retries.cooldowns,
            &mut retries.deferred,
            &mut retries.recorded,
            scope.max_attempts,
            unit.did.clone(),
            collection,
            cursor,
            failure,
            1,
        )
        .await;
    }

    let removed = job_write(scope.state, &scope.queue.job_id, "complete_unit", || {
        complete_unit(scope.state, scope.queue, &unit, scope.pds_endpoint, records)
    })
    .await;
    release_slot(scope.queue, removed);
    publish_event(
        scope.state,
        super::types::BackfillEvent::RepoFetched {
            job_id: scope.queue.job_id.to_string(),
            did: unit.did.clone(),
            pds_endpoint: scope.pds_endpoint.to_string(),
            records_fetched: records,
        },
    );
    let repos = scope.processed_repos.fetch_add(1, Ordering::Relaxed) + 1;
    trim_completions_on_schedule(scope.state, scope.queue, repos).await;
    Some((repos, records_now))
}

/// Count records a deferred retry fetched after its unit completed.
async fn record_late_records(scope: &WorkerScope<'_>, records: i32) {
    if records == 0 {
        return;
    }
    scope.total_records.fetch_add(records, Ordering::Relaxed);
    job_write(scope.state, &scope.queue.job_id, "late_records", || {
        add_late_records(scope.state, scope.queue, scope.pds_endpoint, records)
    })
    .await;
}

/// One unit's first attempt: every collection it covers, each drained from
/// the start.
///
/// Workers spawn this as a task of its own rather than polling it inline.
/// Settling a finished unit awaits database writes, and while it does, the
/// worker polls nothing else. A fetch suspended inside a page's transaction
/// would hold its connection (and on SQLite the write lock) until polled
/// again, so the settle could wait on it forever.
async fn fetch_unit(
    state: Arc<AppState>,
    pds_endpoint: String,
    collections: Vec<String>,
    unit: WorkUnit,
    cancelled: Arc<AtomicBool>,
    requests: Arc<tokio::sync::Semaphore>,
) -> UnitFetchResult {
    let mut count: i32 = 0;
    let mut any_success = false;
    let mut failures: Vec<(String, FetchOutcome)> = Vec::new();
    for collection in &collections {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        // Per collection, not per unit: a unit spanning twenty lexicons must
        // not pin one permit for all twenty sequential request streams.
        let _permit = requests
            .acquire()
            .await
            .expect("request semaphore is never closed");
        match fetch_records_page_loop(
            &state,
            &pds_endpoint,
            &unit.did,
            collection,
            None,
            &cancelled,
        )
        .await
        {
            FetchOutcome::Complete { count: c } => {
                count += c as i32;
                any_success = true;
            }
            outcome @ FetchOutcome::Failed { count: c, .. } => {
                count += c as i32;
                failures.push((collection.clone(), outcome));
            }
        }
    }
    (unit, count, any_success, failures)
}

/// A `fetch_unit` task's result. A task that panicked took its unit with it:
/// the unit stays queued for the next run, and its window slot is freed so
/// this run's discovery is not held up by it.
fn fetched_unit(
    queue: &JobQueue,
    joined: Result<UnitFetchResult, tokio::task::JoinError>,
) -> Option<UnitFetchResult> {
    match joined {
        Ok(result) => Some(result),
        Err(e) => {
            tracing::warn!(job_id = %queue.job_id, error = %e, "backfill unit fetch task panicked; the unit stays queued");
            release_slot(queue, None);
            None
        }
    }
}

async fn run_pds_worker(ctx: FetchContext, pds_endpoint: String, mut rx: mpsc::Receiver<WorkUnit>) {
    let FetchContext {
        state,
        queue,
        collections,
        processed_repos,
        total_records,
        cancelled,
        dids_per_pds,
        recorder,
        requests,
    } = ctx;
    let mut fetches = FuturesUnordered::new();
    let mut rx_open = true;
    let pds_host = profile::host_of(&pds_endpoint);
    let scope = WorkerScope {
        state: &state,
        queue: &queue,
        pds_endpoint: &pds_endpoint,
        pds_host: &pds_host,
        recorder: &recorder,
        max_attempts: load_max_attempts(&state).await,
        processed_repos: &processed_repos,
        total_records: &total_records,
        cancelled: &cancelled,
    };
    // Local to this worker, not job-wide state: every PDS worker owns its own
    // cooldown and queue, keyed to the one host it alone talks to.
    let mut retries = WorkerRetries {
        cooldowns: HostCooldowns::new(),
        deferred: DeferredQueue::new(),
        recorded: RecordedDids::new(),
    };

    loop {
        tokio::select! {
            biased;

            Some(joined) = fetches.next(), if !fetches.is_empty() => {
                let Some(result) = fetched_unit(&queue, joined) else {
                    continue;
                };
                let Some((repos, records)) = finish_unit(&scope, &mut retries, result).await else {
                    break;
                };
                if cancelled.load(Ordering::Relaxed) || should_stop_worker(&state, &queue.job_id).await {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
                publish_event(&state, super::types::BackfillEvent::JobCounters {
                    job_id: queue.job_id.to_string(),
                    total_repos: None,
                    resolved_repos: None,
                    processed_repos: Some(repos),
                    total_records: Some(records),
                });
            }

            unit = rx.recv(), if rx_open && fetches.len() < dids_per_pds => {
                match unit {
                    Some(unit) if !cancelled.load(Ordering::Relaxed) => {
                        // Its own task, not just a future in `fetches`: see
                        // `fetch_unit`.
                        fetches.push(tokio::spawn(fetch_unit(
                            Arc::clone(&state),
                            pds_endpoint.clone(),
                            unit.collections(&collections),
                            unit,
                            Arc::clone(&cancelled),
                            Arc::clone(&requests),
                        )));
                    }
                    _ => {
                        rx_open = false;
                    }
                }
            }

            else => break,
        }
    }

    // Settle whatever is still in flight.
    while let Some(joined) = fetches.next().await {
        if let Some(result) = fetched_unit(&queue, joined) {
            finish_unit(&scope, &mut retries, result).await;
        }
    }

    // Deferred pass. All primary fetches are exhausted, so anything still
    // here is waiting on a clock rather than on work. These units are already
    // complete in the queue; a retry only adds records.
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        match next_drain_step(
            &mut retries.deferred,
            &retries.cooldowns,
            Duration::from_secs(2),
        )
        .await
        {
            DrainStep::Done => break,
            DrainStep::Slept => {
                if should_stop_worker(&state, &queue.job_id).await {
                    cancelled.store(true, Ordering::Relaxed);
                    break;
                }
            }
            DrainStep::Retry(item) => {
                let (did, collection, cursor) = item.payload;
                let attempts = item.attempts + 1;
                // A retry is a request like any other and counts against the
                // same budget, or a job full of retrying workers would ignore
                // the cap entirely.
                let permit = requests
                    .acquire()
                    .await
                    .expect("request semaphore is never closed");
                let outcome = fetch_records_page_loop(
                    &state,
                    &pds_endpoint,
                    &did,
                    &collection,
                    cursor,
                    &cancelled,
                )
                .await;
                drop(permit);
                match outcome {
                    FetchOutcome::Complete { count } => {
                        retries.cooldowns.record_success(&pds_host);
                        record_late_records(&scope, count as i32).await;
                    }
                    FetchOutcome::Failed {
                        count,
                        cursor,
                        failure,
                    } => {
                        record_late_records(&scope, count as i32).await;
                        let last_failure = failure.clone();
                        defer_or_give_up_fetch(
                            &state,
                            &queue,
                            &pds_endpoint,
                            &pds_host,
                            &recorder,
                            &mut retries.cooldowns,
                            &mut retries.deferred,
                            &mut retries.recorded,
                            scope.max_attempts,
                            did,
                            collection,
                            cursor,
                            failure,
                            attempts,
                        )
                        .await;
                        abandon_saturated_pds(
                            &state,
                            &queue,
                            &pds_endpoint,
                            &pds_host,
                            &recorder,
                            &retries.cooldowns,
                            &mut retries.deferred,
                            &mut retries.recorded,
                            &last_failure,
                        )
                        .await;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3: Fetch records from PDS instances (legacy, for resumed jobs)
// ---------------------------------------------------------------------------

async fn run_fetching_phase(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    concurrency: &BackfillConcurrency,
) -> (i32, i32) {
    set_stage(state, job_id, "fetching_records").await;

    // One error sink for the whole job, shared by every PDS below — see
    // `ErrorRecorder`'s doc comment for why a per-worker recorder would be
    // wrong. This function only runs once per job (the alternate, "already
    // resolved" path to `run_pipelined_resolve_and_fetch`), so constructing
    // it once here follows the same one-per-job rule.
    let recorder = Arc::new(super::backfill_errors::ErrorRecorder::new(state, job_id).await);
    let max_attempts = load_max_attempts(state).await;

    // Load pending repos grouped by PDS
    let sql = adapt_sql(
        "SELECT did, pds_endpoint FROM happyview_backfill_repos WHERE job_id = ? AND status = 'pending' AND pds_endpoint IS NOT NULL",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    let mut pds_to_dids: HashMap<String, Vec<String>> = HashMap::new();
    for (did, pds) in rows {
        pds_to_dids.entry(pds).or_default().push(did);
    }

    // Count already-completed repos for accurate progress
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND status = 'completed'",
        state.db_backend,
    );
    let already_completed: i32 = crate::db::query_as::<(i32,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await
        .map(|(c,)| c)
        .unwrap_or(0);

    // Reset processed_repos for the fetching phase
    update_job_counter(state, job_id, "processed_repos", already_completed).await;

    // Seed total_records from DB so a resumed job doesn't lose its prior count
    let existing_records: i32 = {
        let sql = adapt_sql(
            "SELECT total_records FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        crate::db::query_as::<(Option<i32>,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.backfill_db)
            .await
            .map(|(c,)| c.unwrap_or(0))
            .unwrap_or(0)
    };

    let processed_repos = Arc::new(AtomicI32::new(already_completed));
    let total_records = Arc::new(AtomicI32::new(existing_records));
    let cancelled = Arc::new(AtomicBool::new(false));
    let next_flush = Arc::new(AtomicI32::new(
        already_completed + random_batch_threshold(10),
    ));
    let state = Arc::new(state.clone());
    let collections = Arc::new(collections.to_vec());
    let job_id_arc = Arc::new(job_id.to_string());
    let legacy_queue = JobQueue::legacy(job_id);

    let pds_entries: Vec<(String, Vec<String>)> = pds_to_dids.into_iter().collect();

    let dids_per_pds = concurrency.dids_per_pds;
    stream::iter(pds_entries)
        .for_each_concurrent(concurrency.pds, |(pds_endpoint, dids)| {
            let state = Arc::clone(&state);
            let collections = Arc::clone(&collections);
            let processed_repos = Arc::clone(&processed_repos);
            let total_records = Arc::clone(&total_records);
            let cancelled = Arc::clone(&cancelled);
            let next_flush = Arc::clone(&next_flush);
            let job_id = Arc::clone(&job_id_arc);
            let queue = legacy_queue.clone();
            let recorder = Arc::clone(&recorder);

            async move {
                // Local to this PDS, not job-wide state: every PDS here gets
                // its own cooldown and queue, keyed to the one host it alone
                // talks to.
                let pds_host = profile::host_of(&pds_endpoint);
                let mut cooldowns = HostCooldowns::new();
                let mut deferred: DeferredQueue<(String, String, Option<String>)> =
                    DeferredQueue::new();
                let mut recorded: RecordedDids = RecordedDids::new();

                stream::iter(dids)
                    .map(|did| {
                        let state = Arc::clone(&state);
                        let collections = Arc::clone(&collections);
                        let cancelled = Arc::clone(&cancelled);
                        let pds_endpoint = pds_endpoint.clone();

                        // A task of its own for the reason `fetch_unit` gives:
                        // `for_each` below awaits writes while polling nothing
                        // else, and a fetch left suspended inside a page's
                        // transaction would hold its connection meanwhile.
                        tokio::spawn(async move {
                            if cancelled.load(Ordering::Relaxed) {
                                return (did, 0i32, false, Vec::new());
                            }

                            let mut did_records: i32 = 0;
                            let mut any_success = false;
                            let mut failures: Vec<(String, FetchOutcome)> = Vec::new();
                            for collection in collections.iter() {
                                if cancelled.load(Ordering::Relaxed) {
                                    break;
                                }
                                match fetch_records_page_loop(
                                    &state,
                                    &pds_endpoint,
                                    &did,
                                    collection,
                                    None,
                                    &cancelled,
                                )
                                .await
                                {
                                    FetchOutcome::Complete { count } => {
                                        did_records += count as i32;
                                        any_success = true;
                                    }
                                    outcome @ FetchOutcome::Failed { count, .. } => {
                                        did_records += count as i32;
                                        failures.push((collection.clone(), outcome));
                                    }
                                }
                            }
                            (did, did_records, any_success, failures)
                        })
                    })
                    // Fetches for different DIDs on this PDS run concurrently;
                    // `for_each` below still consumes their results one at a
                    // time, which is what lets the cooldown/deferred-queue
                    // bookkeeping below use plain `&mut` instead of a lock.
                    .buffer_unordered(dids_per_pds)
                    .filter_map(|joined| async move {
                        // A panicked fetch leaves its repo pending for the
                        // next run, as a cancelled one does.
                        joined
                            .inspect_err(|e| tracing::warn!(error = %e, "backfill repo fetch task panicked"))
                            .ok()
                    })
                    .for_each(|(did, did_records, any_success, mut failures)| {
                        // `HostCooldowns`/`DeferredQueue` can't be borrowed
                        // into the returned future here — `for_each`'s FnMut
                        // signature doesn't let a captured `&mut` escape into
                        // it (a borrow-checker limitation, not a concurrency
                        // one; `for_each` still drives one future to
                        // completion before calling this closure again). So
                        // the defer/give-up decision is made synchronously
                        // here, before the async block, which only awaits
                        // the give-ups' recorder I/O.
                        total_records.fetch_add(did_records, Ordering::Relaxed);
                        if any_success {
                            cooldowns.record_success(&pds_host);
                        }

                        retryable_give_ups_first(&mut failures);
                        // At most one give-up per DID reaches the recorder —
                        // the detail table is keyed `(job_id, did, phase)`, so
                        // recording once per failed collection inflated
                        // `error_counts` against the rows it was supposed to
                        // summarise. `retryable_give_ups_first` above is what
                        // decides *which* one survives.
                        let mut give_up: Option<(
                            String,
                            String,
                            crate::admin::backfill_errors::BackfillFailure,
                        )> = None;
                        for (collection, outcome) in failures {
                            let FetchOutcome::Failed { cursor, failure, .. } = outcome else {
                                continue;
                            };
                            let attempts = 1;
                            if failure.kind.is_retryable() && attempts < max_attempts {
                                let now = std::time::Instant::now();
                                cooldowns.record_failure(&pds_host, failure.retry_after, now);
                                deferred.push(DeferredItem {
                                    payload: (did.clone(), collection, cursor),
                                    host: pds_host.clone(),
                                    attempts,
                                    eligible_at: now,
                                });
                            } else {
                                // Logged per collection even when only one is
                                // recorded: the log is where an operator finds
                                // out *which* collection failed.
                                tracing::warn!(
                                    did,
                                    collection,
                                    pds = %pds_endpoint,
                                    kind = failure.kind.as_str(),
                                    attempts,
                                    "giving up fetching records from PDS: {}",
                                    failure.message
                                );
                                if give_up.is_none() && recorded.insert(did.clone()) {
                                    give_up = Some((did.clone(), collection, failure));
                                }
                            }
                        }

                        let state = Arc::clone(&state);
                        let processed_repos = Arc::clone(&processed_repos);
                        let total_records = Arc::clone(&total_records);
                        let cancelled = Arc::clone(&cancelled);
                        let next_flush = Arc::clone(&next_flush);
                        let job_id = Arc::clone(&job_id);
                        let recorder = Arc::clone(&recorder);

                        async move {
                            if let Some((did, collection, failure)) = give_up {
                                recorder
                                    .record(
                                        &state,
                                        job_id.as_str(),
                                        &did,
                                        Some(collection.as_str()),
                                        "fetch",
                                        &failure,
                                        1,
                                    )
                                    .await;
                            }

                            // Mark DID as completed
                            let sql = adapt_sql(
                                "UPDATE happyview_backfill_repos SET status = 'completed', records_fetched = ? WHERE job_id = ? AND did = ?",
                                state.db_backend,
                            );
                            job_write(&state, job_id.as_str(), "complete_repo", || {
                                crate::db::query(&sql)
                                    .bind(did_records)
                                    .bind(job_id.as_str())
                                    .bind(&did)
                                    .execute(&state.backfill_db)
                            })
                            .await;

                            let repos = processed_repos.fetch_add(1, Ordering::Relaxed) + 1;
                            let records = total_records.load(Ordering::Relaxed);

                            let threshold = next_flush.load(Ordering::Relaxed);
                            if repos >= threshold
                                && next_flush.compare_exchange(threshold, repos + random_batch_threshold(10), Ordering::Relaxed, Ordering::Relaxed).is_ok()
                            {
                                let backend = state.db_backend;
                                let sql = adapt_sql(
                                    "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
                                    backend,
                                );
                                job_write(&state, job_id.as_str(), "counters", || {
                                    crate::db::query(&sql)
                                        .bind(repos)
                                        .bind(records)
                                        .bind(job_id.as_str())
                                        .execute(&state.backfill_db)
                                })
                                .await;

                                if should_stop_worker(&state, job_id.as_str()).await {
                                    cancelled.store(true, Ordering::Relaxed);
                                }
                            }

                            publish_event(&state, super::types::BackfillEvent::JobCounters {
                                job_id: job_id.to_string(),
                                total_repos: None,
                                resolved_repos: None,
                                processed_repos: Some(repos),
                                total_records: Some(records),
                            });
                        }
                    })
                    .await;

                // Deferred pass. All primary fetches for this PDS are
                // exhausted, so anything still here is waiting on a clock
                // rather than on work.
                loop {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    match next_drain_step(&mut deferred, &cooldowns, Duration::from_secs(2)).await
                    {
                        DrainStep::Done => break,
                        DrainStep::Slept => {
                            if should_stop_worker(&state, job_id.as_str()).await {
                                cancelled.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                        DrainStep::Retry(item) => {
                            let (did, collection, cursor) = item.payload;
                            let attempts = item.attempts + 1;
                            match fetch_records_page_loop(
                                &state,
                                &pds_endpoint,
                                &did,
                                &collection,
                                cursor,
                                &cancelled,
                            )
                            .await
                            {
                                FetchOutcome::Complete { count } => {
                                    cooldowns.record_success(&pds_host);
                                    total_records.fetch_add(count as i32, Ordering::Relaxed);
                                }
                                FetchOutcome::Failed { count, cursor, failure } => {
                                    total_records.fetch_add(count as i32, Ordering::Relaxed);
                                    let last_failure = failure.clone();
                                    defer_or_give_up_fetch(
                                        &state,
                                        &queue,
                                        &pds_endpoint,
                                        &pds_host,
                                        &recorder,
                                        &mut cooldowns,
                                        &mut deferred,
                                        &mut recorded,
                                        max_attempts,
                                        did,
                                        collection,
                                        cursor,
                                        failure,
                                        attempts,
                                    )
                                    .await;
                                    abandon_saturated_pds(
                                        &state,
                                        &queue,
                                        &pds_endpoint,
                                        &pds_host,
                                        &recorder,
                                        &cooldowns,
                                        &mut deferred,
                                        &mut recorded,
                                        &last_failure,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
            }
        })
        .await;

    recorder.flush(&state, job_id).await;

    let final_repos = processed_repos.load(Ordering::Relaxed);
    let final_records = total_records.load(Ordering::Relaxed);

    // Persist final counts so they're accurate regardless of batch size
    let sql = adapt_sql(
        "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
        state.db_backend,
    );
    job_write(&state, job_id, "counters", || {
        crate::db::query(&sql)
            .bind(final_repos)
            .bind(final_records)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;

    (final_repos, final_records)
}

struct PreparedRecord {
    uri: String,
    did: String,
    collection: String,
    rkey: String,
    record_json: String,
    cid: String,
}

/// Write one `listRecords` page in one transaction: the multi-row upsert, then
/// refs for exactly the rows it inserted or changed. The upsert's `WHERE`
/// skips unchanged rows, which keep their refs and `indexed_at`. Returns the
/// URIs written.
async fn write_records_page(
    state: &AppState,
    batch: &[PreparedRecord],
) -> Result<Vec<String>, sqlx::Error> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }
    let backend = state.db_backend;
    let now = now_rfc3339();

    // 8 params per row; a page is at most 100 rows, under SQLite's 999.
    let placeholders = vec!["(?, ?, ?, ?, ?, ?, ?, ?)"; batch.len()].join(", ");
    let upsert_sql = adapt_sql(
        &format!(
            "INSERT INTO happyview_records (uri, did, collection, rkey, record, cid, indexed_at, created_at) VALUES {placeholders} \
             ON CONFLICT (uri) DO UPDATE SET record = EXCLUDED.record, cid = EXCLUDED.cid, indexed_at = EXCLUDED.indexed_at \
             WHERE {} RETURNING uri",
            crate::db::record_changed_clause(backend)
        ),
        backend,
    );

    let mut tx = state.backfill_db.begin().await?;
    let mut upsert = crate::db::query_as::<(String,)>(&upsert_sql);
    for rec in batch {
        upsert = upsert
            .bind(&rec.uri)
            .bind(&rec.did)
            .bind(&rec.collection)
            .bind(&rec.rkey)
            .bind(&rec.record_json)
            .bind(&rec.cid)
            .bind(&now)
            .bind(&now);
    }
    let written: Vec<String> = upsert
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .map(|(uri,)| uri)
        .collect();

    if !written.is_empty() {
        let delete_sql = adapt_sql(
            &format!(
                "DELETE FROM happyview_record_refs WHERE source_uri IN ({})",
                vec!["?"; written.len()].join(", ")
            ),
            backend,
        );
        let mut delete = crate::db::query(&delete_sql);
        for uri in &written {
            delete = delete.bind(uri.as_str());
        }
        delete.execute(&mut *tx).await?;

        let written_uris: HashSet<&str> = written.iter().map(String::as_str).collect();
        let mut refs: Vec<(&str, String, &str)> = Vec::new();
        for rec in batch
            .iter()
            .filter(|rec| written_uris.contains(rec.uri.as_str()))
        {
            let value: Value = serde_json::from_str(&rec.record_json).unwrap_or_default();
            for target in crate::record_refs::extract_at_uris(&value) {
                refs.push((&rec.uri, target, &rec.collection));
            }
        }
        for chunk in refs.chunks(crate::record_refs::REFS_PER_INSERT) {
            let ref_sql = adapt_sql(
                &format!(
                    "INSERT INTO happyview_record_refs (source_uri, target_uri, collection) VALUES {} ON CONFLICT DO NOTHING",
                    vec!["(?, ?, ?)"; chunk.len()].join(", ")
                ),
                backend,
            );
            let mut insert = crate::db::query(&ref_sql);
            for (source, target, collection) in chunk {
                insert = insert.bind(*source).bind(target.as_str()).bind(*collection);
            }
            insert.execute(&mut *tx).await?;
        }
    }

    tx.commit().await?;
    Ok(written)
}

/// Fetch labels for freshly written records, when any labeler is subscribed.
async fn queue_label_backfill(state: &AppState, uris: &[String]) {
    if uris.is_empty() || !crate::labeler::has_active_subscriptions(state).await {
        return;
    }
    let shared = Arc::new(state.clone());
    for uri in uris {
        crate::labeler::backfill_labels_for_uri(Arc::clone(&shared), uri.clone());
    }
}

/// The outcome of a records-page-loop attempt.
///
/// `Failed` carries the cursor reached so far so a retry can resume
/// mid-pagination instead of restarting the DID's collection from page one.
pub(super) enum FetchOutcome {
    Complete {
        count: u32,
    },
    Failed {
        count: u32,
        // Read by the deferred-retry wiring in both fetch call sites, to
        // resume mid-pagination instead of restarting the DID's collection.
        cursor: Option<String>,
        failure: crate::admin::backfill_errors::BackfillFailure,
    },
}

/// Fetch all records for a given DID and collection from a PDS via
/// `com.atproto.repo.listRecords`, paginating from `start_cursor`.
///
/// This is a single attempt at draining the collection: it never sleeps on a
/// rate limit and never retries a transport or server error. It returns
/// `Failed` with the cursor reached so far so the caller can defer and resume.
async fn fetch_records_page_loop(
    state: &AppState,
    pds_endpoint: &str,
    did: &str,
    collection: &str,
    start_cursor: Option<String>,
    cancelled: &AtomicBool,
) -> FetchOutcome {
    let base = pds_endpoint.trim_end_matches('/');
    let mut cursor: Option<String> = start_cursor;
    let mut count: u32 = 0;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        let mut url = format!(
            "{base}/xrpc/com.atproto.repo.listRecords?repo={did}&collection={collection}&limit=100"
        );
        if let Some(ref c) = cursor {
            url.push_str(&format!("&cursor={c}"));
        }

        let resp = match state.http.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::admin::backfill_errors::BackfillFailure::from_reqwest(&e),
                };
            }
        };

        if !resp.status().is_success() {
            // The body is the only thing that distinguishes the common cases —
            // a PDS answers `400 InvalidRequest / Could not find repo` for an
            // account that has been deleted or migrated away, which is routine
            // during backfill and not worth investigating. Without it, every
            // non-2xx reads identically.
            let status = resp.status();
            let headers = resp.headers().clone();
            let body = resp.text().await.unwrap_or_default();
            return FetchOutcome::Failed {
                count,
                cursor,
                failure: crate::admin::backfill_errors::BackfillFailure::from_pds_response(
                    status, &body, &headers,
                ),
            };
        }

        let body: ListRecordsResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::admin::backfill_errors::BackfillFailure {
                        kind: crate::admin::backfill_errors::BackfillErrorKind::Other,
                        message: format!("invalid PDS response: {e}"),
                        retry_after: None,
                    },
                };
            }
        };

        let page_count = body.records.len();

        let mut batch: Vec<PreparedRecord> = Vec::with_capacity(page_count);
        for entry in &body.records {
            let rkey = entry.uri.rsplit('/').next().unwrap_or_default().to_string();
            let uri = format!("at://{did}/{collection}/{rkey}");

            // Reject records whose claimed CID doesn't match their content
            // (security review L9). The backfill source PDS is attacker-
            // controllable via the DID document, so a hostile PDS could serve a
            // record under a mismatched CID. Skip on mismatch; `Skipped`
            // (unencodable value) proceeds unchanged.
            if crate::cid_verify::verify_record_cid(&entry.cid, &entry.value)
                == crate::cid_verify::CidCheck::Mismatch
            {
                tracing::warn!(
                    collection,
                    did,
                    rkey,
                    claimed_cid = %entry.cid,
                    "record content does not match claimed CID, skipping"
                );
                continue;
            }

            let rec_to_store = match crate::lua::run_record_event_script(
                state,
                crate::lua::RecordEventPayload {
                    nsid: collection,
                    action: "create",
                    uri: &uri,
                    did,
                    rkey: &rkey,
                    record: Some(&entry.value),
                },
            )
            .await
            {
                crate::lua::RecordHookOutcome::Skip => continue,
                crate::lua::RecordHookOutcome::Replace(v) => v,
                crate::lua::RecordHookOutcome::Proceed => entry.value.clone(),
            };

            batch.push(PreparedRecord {
                uri,
                did: did.to_string(),
                collection: collection.to_string(),
                rkey,
                record_json: serde_json::to_string(&rec_to_store).unwrap_or_default(),
                cid: entry.cid.clone(),
            });
        }

        match crate::db::retry_on_busy(|| write_records_page(state, &batch)).await {
            Ok(written) => {
                count += batch.len() as u32;
                queue_label_backfill(state, &written).await;
            }
            Err(e) => {
                tracing::error!(did, collection, error = %e, "failed to write a page of backfilled records");
                // The page rolled back, so it is not counted, and a retry
                // resumes from the cursor that fetched it.
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::admin::backfill_errors::BackfillFailure {
                        kind: crate::admin::backfill_errors::BackfillErrorKind::Other,
                        message: format!("database write failed: {e}"),
                        retry_after: None,
                    },
                };
            }
        }

        match body.cursor {
            Some(c) if page_count > 0 => cursor = Some(c),
            _ => break,
        }
    }

    FetchOutcome::Complete { count }
}

// ---------------------------------------------------------------------------
// Background backfill worker
// ---------------------------------------------------------------------------

async fn run_backfill_job(state: AppState, job_id: String) {
    let window = parse_discovery_window(std::env::var("BACKFILL_DISCOVERY_WINDOW").ok().as_deref());
    run_backfill_job_with(state, job_id, window).await;
}

async fn run_backfill_job_with(state: AppState, job_id: String, window: i64) {
    let backend = state.db_backend;

    // Load job metadata
    let sql = adapt_sql(
        "SELECT collection, did, stage, queue_version, discovery_complete FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let job: Option<(Option<String>, Option<String>, String, i32, i32)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten();

    let Some((collection, did, stage, queue_version, discovery_complete)) = job else {
        tracing::error!(job_id, "backfill job not found");
        return;
    };

    // Determine target collections
    let collections: Vec<String> = if let Some(ref col) = collection {
        let lexicon_exists: bool = state
            .lexicons
            .get(col)
            .await
            .is_some_and(|lex| lex.lexicon_type == crate::lexicon::LexiconType::Record);
        if !lexicon_exists {
            let error = format!("no record-type lexicon registered for collection '{col}'");
            fail_job(&state, &job_id, &error).await;
            return;
        }
        vec![col.clone()]
    } else {
        let sql = adapt_sql(
            "SELECT id FROM happyview_lexicons WHERE json_extract(lexicon_json, '$.defs.main.type') = 'record'",
            backend,
        );
        let rows: Vec<(String,)> = match crate::db::query_as(&sql)
            .fetch_all(&state.backfill_db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                let error = format!("failed to query backfill-eligible lexicons: {e}");
                fail_job(&state, &job_id, &error).await;
                return;
            }
        };
        rows.into_iter().map(|(id,)| id).collect()
    };

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

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.completed".to_string(),
            severity: Severity::Info,
            actor_did: None,
            subject: collection,
            detail: serde_json::json!({
                "job_id": job_id,
                "total_repos": final_processed,
                "total_records": final_records,
            }),
        },
        backend,
    )
    .await;
}

/// Pause a bounded job that ran out of work to do with units still queued,
/// rather than mark it completed with work left.
///
/// The reason goes in the job's `error`, so the dashboard can tell this from
/// a pause an operator asked for. Resuming clears it.
async fn pause_with_units_left(
    state: &AppState,
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
        crate::db::query(&sql)
            .bind(&reason)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;
    publish_event(
        state,
        super::types::BackfillEvent::JobCompleted {
            job_id: job_id.to_string(),
            status: "paused".to_string(),
            error: Some(reason.clone()),
        },
    );
    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.units_left".to_string(),
            severity: Severity::Warn,
            actor_did: None,
            subject: collection.map(str::to_string),
            detail: serde_json::json!({
                "job_id": job_id,
                "queued_units": left,
                "reason": reason,
            }),
        },
        state.db_backend,
    )
    .await;
}

/// Work through a job's queue, discovering more alongside while discovery is
/// unfinished. Returns the job's `(processed_repos, total_records)`, or the
/// error the job fails with: a queue that could not be read or written, by
/// discovery or by the pipeline, never lets the job finish as completed.
async fn run_queue(
    state: &AppState,
    job_id: &str,
    collections: &[String],
    concurrency: &BackfillConcurrency,
    version: QueueVersion,
    discovery_complete: bool,
    window: i64,
) -> Result<(i32, i32), String> {
    let queued = match version {
        QueueVersion::Legacy => 0,
        QueueVersion::Bounded => queued_units(state, job_id)
            .await
            .map_err(|e| format!("failed to read the backfill queue: {e}"))?,
    };
    let queue = JobQueue {
        job_id: Arc::new(job_id.to_string()),
        version,
        window: Arc::new(QueueWindow::new(window, queued)),
        discovery_done: Arc::new(AtomicBool::new(
            version == QueueVersion::Legacy || discovery_complete,
        )),
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let discovery = (!queue.discovery_done.load(Ordering::Acquire)).then(|| {
        tokio::spawn(run_discovery(
            state.clone(),
            queue.clone(),
            collections.to_vec(),
            Arc::clone(&cancelled),
        ))
    });
    let counts =
        run_pipelined_resolve_and_fetch(state, collections, concurrency, &queue, &cancelled).await;
    if let Some(handle) = discovery {
        // Discovery's error comes first: when it fails it stops the pipeline,
        // whose own result then says nothing useful.
        handle
            .await
            .map_err(|e| format!("backfill discovery task panicked: {e}"))??;
    }
    counts
}

// ---------------------------------------------------------------------------
// Admin handlers
// ---------------------------------------------------------------------------

/// POST /admin/backfill — create a backfill job and spawn background work.
pub(super) async fn create_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Json(body): Json<CreateBackfillBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let mut inputs = body.dids.unwrap_or_default();
    inputs.extend(body.did);
    let dids = resolve_backfill_accounts(inputs, |input| async move {
        crate::identity::resolve_identifier(&input)
            .await
            .map(|resolved| resolved.did)
    })
    .await?;

    let job_id = start_backfill(&state, body.collection, dids, &admin.did).await?;

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "id": job_id,
            "status": "running",
        })),
    ))
}

pub(crate) const MAX_BACKFILL_ACCOUNTS: usize = 500;

/// Resolve every requested account to a DID, all or nothing.
///
/// A partial job would silently skip accounts the operator asked for, so one
/// bad entry fails the request and the error lists every bad entry at once.
/// The cap is checked before resolving so an oversized request costs no
/// lookups.
///
/// An empty `inputs` means no accounts were asked for, which is a network
/// backfill. Entries that are all blank are refused instead: treating them as
/// "no accounts" would turn a malformed targeted request into a backfill of
/// the whole network.
pub(crate) async fn resolve_backfill_accounts<F, Fut>(
    inputs: Vec<String>,
    resolve: F,
) -> Result<Vec<String>, AppError>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, AppError>>,
{
    let supplied_any = !inputs.is_empty();
    let mut seen_inputs = HashSet::new();
    let unique: Vec<String> = inputs
        .into_iter()
        .map(|input| input.trim().to_string())
        .filter(|input| !input.is_empty() && seen_inputs.insert(input.clone()))
        .collect();

    if supplied_any && unique.is_empty() {
        return Err(AppError::BadRequest(
            "no valid accounts given: every entry was blank".into(),
        ));
    }

    if unique.len() > MAX_BACKFILL_ACCOUNTS {
        return Err(AppError::BadRequest(format!(
            "a backfill can target at most {MAX_BACKFILL_ACCOUNTS} accounts, got {}",
            unique.len()
        )));
    }

    let results: Vec<Result<String, AppError>> = stream::iter(unique)
        .map(&resolve)
        .buffered(8)
        .collect()
        .await;

    let mut seen_dids = HashSet::new();
    let mut dids = Vec::new();
    let mut failures = Vec::new();
    for result in results {
        match result {
            Ok(did) => {
                if seen_dids.insert(did.clone()) {
                    dids.push(did);
                }
            }
            Err(AppError::BadRequest(msg)) => failures.push(msg),
            Err(other) => failures.push(other.to_string()),
        }
    }

    if !failures.is_empty() {
        return Err(AppError::BadRequest(format!(
            "could not resolve {} account(s): {}",
            failures.len(),
            failures.join("; ")
        )));
    }

    Ok(dids)
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
async fn insert_targeted_job(
    state: &AppState,
    job_id: &str,
    collection: Option<&str>,
    dids: &[String],
) -> Result<(), AppError> {
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
        .map_err(|e| AppError::Internal(format!("failed to begin transaction: {e}")))?;

    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, queue_version, discovery_complete, started_at, created_at) \
         VALUES (?, ?, ?, 'dids', 'running', 'resolving_pds', ?, 2, 1, ?, ?)",
        backend,
    );
    crate::db::query(&sql)
        .bind(job_id)
        .bind(collection)
        .bind(single_did)
        .bind(total)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create backfill job: {e}")))?;

    for chunk in dids.chunks(DISCOVERY_INSERT_CHUNK) {
        let placeholders = vec!["(?, ?, ?)"; chunk.len()].join(", ");
        let sql_str = format!(
            "INSERT INTO happyview_backfill_queue (job_id, collection, did) VALUES {placeholders} ON CONFLICT DO NOTHING",
        );
        let sql = adapt_sql(&sql_str, backend);
        let mut insert = crate::db::query(&sql);
        for did in chunk {
            insert = insert.bind(job_id).bind(ALL_COLLECTIONS).bind(did);
        }
        insert
            .execute(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(format!("failed to seed backfill units: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(format!("failed to commit backfill job: {e}")))
}

/// Insert a backfill job without starting it. With no `dids` the job
/// discovers repos through the relay; otherwise it targets exactly those.
pub(crate) async fn create_backfill_job(
    state: &AppState,
    collection: Option<&str>,
    dids: &[String],
) -> Result<String, AppError> {
    let job_id = Uuid::new_v4().to_string();

    if !dids.is_empty() {
        insert_targeted_job(state, &job_id, collection, dids).await?;
        return Ok(job_id);
    }

    let now = now_rfc3339();
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_jobs \
         (id, collection, did, scope, status, stage, total_repos, queue_version, discovery_complete, started_at, created_at) \
         VALUES (?, ?, NULL, 'network', 'running', 'pending', 0, 2, 0, ?, ?)",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(&job_id)
        .bind(collection)
        .bind(&now)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to create backfill job: {e}")))?;

    Ok(job_id)
}

pub(crate) async fn start_backfill(
    state: &AppState,
    collection: Option<String>,
    dids: Vec<String>,
    actor_did: &str,
) -> Result<String, AppError> {
    let job_id = create_backfill_job(state, collection.as_deref(), &dids).await?;
    let scope = if dids.is_empty() { "network" } else { "dids" };

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.started".to_string(),
            severity: Severity::Info,
            actor_did: Some(actor_did.to_string()),
            subject: collection.clone(),
            detail: serde_json::json!({
                "job_id": job_id.clone(),
                "scope": scope,
                "account_count": dids.len(),
            }),
        },
        state.db_backend,
    )
    .await;

    let spawn_state = state.clone();
    let spawn_job_id = job_id.clone();
    tokio::spawn(async move {
        run_backfill_job(spawn_state, spawn_job_id).await;
    });

    Ok(job_id)
}

/// POST /admin/backfill/{id}/cancel — cancel a running backfill job.
pub(super) async fn cancel_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "cancelling" || status == "cancelled" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((ref status,)) if status == "paused" => {
            finalise_cancel(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.cancelled".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelled" }),
            ))
        }
        Some((status,)) if status != "running" => Err(AppError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_cancel(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.cancelling".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "cancelling" }),
            ))
        }
    }
}

/// POST /admin/backfill/{id}/pause — pause a running backfill job.
pub(super) async fn pause_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((ref status,)) if status == "pausing" || status == "paused" => {
            Ok(Json(serde_json::json!({ "id": job_id, "status": status })))
        }
        Some((status,)) if status != "running" => Err(AppError::BadRequest(format!(
            "job is not running (status: {status})"
        ))),
        Some(_) => {
            request_pause(&state, &job_id).await;
            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.pausing".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "pausing" }),
            ))
        }
    }
}

/// POST /admin/backfill/{id}/resume — resume a paused backfill job.
pub(super) async fn resume_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
) -> Result<Json<Value>, AppError> {
    admin.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "SELECT status FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let row: Option<(String,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    match row {
        None => Err(AppError::NotFound("backfill job not found".into())),
        Some((status,)) if status != "paused" => Err(AppError::BadRequest(format!(
            "job is not paused (status: {status})"
        ))),
        Some(_) => {
            let sql = adapt_sql(
                "UPDATE happyview_backfill_jobs SET status = 'running', error = NULL WHERE id = ?",
                state.db_backend,
            );
            crate::db::retry_on_busy(|| {
                crate::db::query(&sql)
                    .bind(&job_id)
                    .execute(&state.backfill_db)
            })
            .await
            .map_err(|e| AppError::Internal(format!("failed to resume backfill job: {e}")))?;

            let spawn_state = state.clone();
            let spawn_job_id = job_id.clone();
            tokio::spawn(async move {
                run_backfill_job(spawn_state, spawn_job_id).await;
            });

            log_event(
                &state.db,
                EventLog {
                    event_type: "backfill.resumed".to_string(),
                    severity: Severity::Info,
                    actor_did: Some(admin.did.clone()),
                    subject: None,
                    detail: serde_json::json!({ "job_id": job_id }),
                },
                state.db_backend,
            )
            .await;
            Ok(Json(
                serde_json::json!({ "id": job_id, "status": "running" }),
            ))
        }
    }
}

/// GET /admin/backfill/status — list all backfill jobs.
pub(super) async fn backfill_status(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<Json<Vec<BackfillJob>>, AppError> {
    auth.require(Permission::BackfillRead).await?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT id, collection, did, scope, status, stage, total_repos, resolved_repos, processed_repos, total_records, error, started_at, completed_at, created_at FROM happyview_backfill_jobs ORDER BY created_at DESC",
        backend,
    );
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    )> = crate::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to list backfill jobs: {e}")))?;

    let jobs: Vec<BackfillJob> = rows
        .into_iter()
        .map(
            |(
                id,
                collection,
                did,
                scope,
                status,
                stage,
                total_repos,
                resolved_repos,
                processed_repos,
                total_records,
                error,
                started_at,
                completed_at,
                created_at,
            )| {
                BackfillJob {
                    id,
                    collection,
                    did,
                    scope,
                    status,
                    stage,
                    total_repos,
                    resolved_repos,
                    processed_repos,
                    total_records,
                    error,
                    started_at,
                    completed_at,
                    created_at,
                }
            },
        )
        .collect();

    Ok(Json(jobs))
}

// ---------------------------------------------------------------------------
// SSE events endpoint
// ---------------------------------------------------------------------------

pub(super) async fn backfill_events(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<
    axum::response::sse::Sse<
        impl futures_util::Stream<Item = Result<axum::response::sse::Event, std::convert::Infallible>>,
    >,
    AppError,
> {
    auth.require(Permission::BackfillRead).await?;

    let mut rx = state.backfill_events_tx.subscribe();

    let stream = async_stream::stream! {
        #[allow(clippy::collapsible_if)]
        if let Some(snapshot) = build_job_snapshot(&state, &job_id).await {
            if let Ok(json) = serde_json::to_string(&snapshot) {
                yield Ok(axum::response::sse::Event::default().event("event").data(json));
            }
        } else {
            tracing::warn!(job_id, "could not build initial job snapshot for SSE client");
        }

        loop {
            match rx.recv().await {
                Ok(event) => {
                    let event_job_id = match &event {
                        super::types::BackfillEvent::RepoDiscovered { job_id, .. }
                        | super::types::BackfillEvent::RepoResolved { job_id, .. }
                        | super::types::BackfillEvent::RepoFetched { job_id, .. }
                        | super::types::BackfillEvent::JobCounters { job_id, .. }
                        | super::types::BackfillEvent::JobStageChanged { job_id, .. }
                        | super::types::BackfillEvent::JobCompleted { job_id, .. }
                        | super::types::BackfillEvent::JobSnapshot { job_id, .. } => job_id,
                    };
                    if *event_job_id != job_id {
                        continue;
                    }
                    if let Ok(json) = serde_json::to_string(&event) {
                        yield Ok(axum::response::sse::Event::default().event("event").data(json));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(job_id, skipped = n, "SSE client lagged behind, resyncing");
                    // Dropped deltas are unrecoverable; a snapshot is the only way back to
                    // a correct view.
                    #[allow(clippy::collapsible_if)]
                    if let Some(snapshot) = build_job_snapshot(&state, &job_id).await {
                        if let Ok(json) = serde_json::to_string(&snapshot) {
                            yield Ok(axum::response::sse::Event::default().event("event").data(json));
                        }
                    }
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Ok(axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}

// ---------------------------------------------------------------------------
// REST detail endpoints
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct ReposQuery {
    phase: Option<String>,
    cursor: Option<String>,
    limit: Option<i32>,
}

pub(super) async fn backfill_repos(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
    axum::extract::Query(query): axum::extract::Query<ReposQuery>,
) -> Result<Json<super::types::BackfillReposResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

    let limit = query.limit.unwrap_or(50).min(100);
    let phase_filter = match query.phase.as_deref() {
        Some("resolved") => " AND pds_endpoint IS NOT NULL",
        Some("fetched") => " AND status = 'completed'",
        _ => "",
    };
    let cursor_filter = if query.cursor.is_some() {
        " AND did > ?"
    } else {
        ""
    };

    let sql_str = format!(
        "SELECT did, pds_endpoint, status, records_fetched FROM happyview_backfill_repos WHERE job_id = ?{phase_filter}{cursor_filter} ORDER BY did ASC LIMIT ?",
    );
    let sql = adapt_sql(&sql_str, state.db_backend);

    let mut q = crate::db::query_as::<(String, Option<String>, String, i32)>(&sql).bind(&job_id);
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit + 1);

    let rows: Vec<(String, Option<String>, String, i32)> = q
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill repos: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let repos: Vec<super::types::BackfillRepoEntry> = rows
        .into_iter()
        .take(limit as usize)
        .map(
            |(did, pds_endpoint, status, records_fetched)| super::types::BackfillRepoEntry {
                did,
                pds_endpoint,
                status,
                records_fetched,
            },
        )
        .collect();

    let cursor = if has_more {
        repos.last().map(|r| r.did.clone())
    } else {
        None
    };

    Ok(Json(super::types::BackfillReposResponse { repos, cursor }))
}

pub(super) async fn backfill_pds_summary(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<Json<super::types::PdsSummaryResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

    let sql = adapt_sql(
        "SELECT pds_endpoint, COUNT(*) as total_repos, SUM(CASE WHEN status = 'completed' THEN 1 ELSE 0 END) as completed_repos, SUM(records_fetched) as total_records FROM happyview_backfill_repos WHERE job_id = ? AND pds_endpoint IS NOT NULL GROUP BY pds_endpoint ORDER BY COUNT(*) DESC",
        state.db_backend,
    );

    let rows: Vec<(String, i32, i32, i64)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query PDS summary: {e}")))?;

    let pds_endpoints: Vec<super::types::PdsSummaryEntry> = rows
        .into_iter()
        .map(
            |(pds_endpoint, total_repos, completed_repos, total_records)| {
                super::types::PdsSummaryEntry {
                    pds_endpoint,
                    total_repos,
                    completed_repos,
                    total_records: total_records as i32,
                }
            },
        )
        .collect();

    Ok(Json(super::types::PdsSummaryResponse { pds_endpoints }))
}

#[derive(Deserialize)]
pub(super) struct BackfillErrorsQuery {
    kind: Option<String>,
    cursor: Option<String>,
    limit: Option<i32>,
}

/// GET /admin/backfill/{id}/errors — paginated failure detail plus exact
/// per-kind totals.
///
/// Named `..._list` rather than `backfill_errors`, which is already the SSE
/// stream handler's name.
pub(super) async fn backfill_errors_list(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
    axum::extract::Query(query): axum::extract::Query<BackfillErrorsQuery>,
) -> Result<Json<BackfillErrorsResponse>, AppError> {
    auth.require(Permission::BackfillRead).await?;

    // Clamped on both ends: SQLite reads a negative LIMIT as "no limit", which
    // would return the whole job's error set (up to ERROR_DETAIL_CAP rows) in
    // one response.
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let kind_filter = if query.kind.is_some() {
        " AND kind = ?"
    } else {
        ""
    };
    // Keyset pagination on `did` alone assumes at most one row per
    // (job_id, did) — the primary key is actually (job_id, did, phase), so a
    // DID with rows in two phases would let `did > ?` skip one at a page
    // boundary. That can't happen today: a DID that fails at `resolve` never
    // reaches `fetch`, and repeat `fetch` failures upsert into the same row.
    // That's a property of the current failure state machine, not of the
    // schema — a future change there could silently start skipping rows.
    let cursor_filter = if query.cursor.is_some() {
        " AND did > ?"
    } else {
        ""
    };

    let sql_str = format!(
        "SELECT did, collection, phase, kind, message, attempts, last_at \
         FROM happyview_backfill_errors WHERE job_id = ?{kind_filter}{cursor_filter} \
         ORDER BY did ASC LIMIT ?",
    );
    let sql = adapt_sql(&sql_str, state.db_backend);

    #[allow(clippy::type_complexity)]
    let mut q =
        crate::db::query_as::<(String, Option<String>, String, String, String, i32, String)>(&sql)
            .bind(&job_id);
    if let Some(ref kind) = query.kind {
        q = q.bind(kind);
    }
    if let Some(ref cursor) = query.cursor {
        q = q.bind(cursor);
    }
    q = q.bind(limit + 1);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Option<String>, String, String, String, i32, String)> = q
        .fetch_all(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill errors: {e}")))?;

    let has_more = rows.len() > limit as usize;
    let errors: Vec<BackfillErrorEntry> = rows
        .into_iter()
        .take(limit as usize)
        .map(
            |(did, collection, phase, kind, message, attempts, last_at)| BackfillErrorEntry {
                did,
                collection,
                phase,
                kind,
                message,
                attempts,
                last_at,
            },
        )
        .collect();

    let cursor = if has_more {
        errors.last().map(|e| e.did.clone())
    } else {
        None
    };

    let error_counts = load_live_error_counts(&state, &job_id).await;
    let counts: Vec<BackfillErrorCount> = BackfillErrorKind::all()
        .into_iter()
        .filter_map(|kind| {
            let count = error_counts.get(kind);
            if count == 0 {
                return None;
            }
            Some(BackfillErrorCount {
                kind: kind.as_str().to_string(),
                count,
                retryable: kind.is_retryable(),
            })
        })
        .collect();
    let capped = error_counts.total() >= ERROR_DETAIL_CAP;

    Ok(Json(BackfillErrorsResponse {
        errors,
        cursor,
        counts,
        capped,
        cap: ERROR_DETAIL_CAP,
    }))
}

/// Per-kind counts for a job, reconciling the two partial views of them.
///
/// `error_counts` on the job row is only written by `ErrorRecorder::flush`, and
/// every flush site is terminal — so for the whole of a multi-hour backfill it
/// reads as empty and the dashboard shows no Errors row at all, and a crash
/// loses everything accumulated since the last flush while the detail rows it
/// summarises survive. Counting the detail table fixes both, and is exact below
/// `ERROR_DETAIL_CAP`. Above the cap rows stop being written and the JSON is the
/// only source left, so take whichever is larger per kind rather than either
/// one alone.
async fn load_live_error_counts(state: &AppState, job_id: &str) -> ErrorCounts {
    let mut counts = load_error_counts(state, job_id).await;

    let sql = adapt_sql(
        "SELECT kind, COUNT(*) FROM happyview_backfill_errors WHERE job_id = ? GROUP BY kind",
        state.db_backend,
    );
    let rows: Vec<(String, i64)> = crate::db::query_as(&sql)
        .bind(job_id)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    for (kind, count) in rows {
        // An unrecognised kind is skipped, not fatal — same rule as
        // `ErrorCounts::from_json`.
        if let Some(kind) = BackfillErrorKind::parse(&kind) {
            counts.raise_to(kind, count);
        }
    }

    counts
}

async fn load_error_counts(state: &AppState, job_id: &str) -> ErrorCounts {
    let sql = adapt_sql(
        "SELECT error_counts FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    crate::db::query_as::<(Option<String>,)>(&sql)
        .bind(job_id)
        .fetch_optional(&state.backfill_db)
        .await
        .ok()
        .flatten()
        .and_then(|(json,)| json)
        .and_then(|json| serde_json::from_str(&json).ok())
        .map(|v| ErrorCounts::from_json(&v))
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub(super) struct RetryFailedBody {
    kinds: Option<Vec<String>>,
}

/// POST /admin/backfill/{id}/retry-failed — spawn a new job scoped to just
/// the failed DIDs from `job_id`.
///
/// A new job rather than mutating the finished one, seeded with
/// `stage = 'resolving_pds'` so `run_backfill_job` skips discovery (its DIDs
/// are already known) and enters the resolve/fetch pipeline directly.
pub(super) async fn retry_failed_backfill(
    State(state): State<AppState>,
    admin: UserAuth,
    Path(job_id): Path<String>,
    Json(body): Json<RetryFailedBody>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    admin.require(Permission::BackfillCreate).await?;
    let backend = state.db_backend;

    let sql = adapt_sql(
        "SELECT collection FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let row: Option<(Option<String>,)> = crate::db::query_as(&sql)
        .bind(&job_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query backfill job: {e}")))?;

    let Some((collection,)) = row else {
        return Err(AppError::NotFound("backfill job not found".into()));
    };

    let kinds: Vec<&'static str> = match &body.kinds {
        Some(requested) if !requested.is_empty() => requested
            .iter()
            .filter_map(|s| BackfillErrorKind::parse(s))
            .map(BackfillErrorKind::as_str)
            .collect(),
        _ => BackfillErrorKind::all()
            .into_iter()
            .filter(|k| k.is_retryable())
            .map(BackfillErrorKind::as_str)
            .collect(),
    };

    if kinds.is_empty() {
        return Err(AppError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let placeholders = vec!["?"; kinds.len()].join(", ");
    let sql_str = format!(
        "SELECT DISTINCT did FROM happyview_backfill_errors WHERE job_id = ? AND kind IN ({placeholders})",
    );
    let sql = adapt_sql(&sql_str, backend);
    let mut q = crate::db::query_as::<(String,)>(&sql).bind(&job_id);
    for kind in &kinds {
        q = q.bind(*kind);
    }
    let dids: Vec<(String,)> = q
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to query failed DIDs: {e}")))?;

    if dids.is_empty() {
        return Err(AppError::BadRequest(
            "no retryable failures for this job".into(),
        ));
    }

    let dids: Vec<String> = dids.into_iter().map(|(did,)| did).collect();
    let new_job_id = Uuid::new_v4().to_string();
    insert_targeted_job(&state, &new_job_id, collection.as_deref(), &dids).await?;

    log_event(
        &state.db,
        EventLog {
            event_type: "backfill.retry_started".to_string(),
            severity: Severity::Info,
            actor_did: Some(admin.did.clone()),
            subject: collection.clone(),
            detail: serde_json::json!({
                "job_id": new_job_id,
                "source_job_id": job_id,
                "retried_repos": dids.len(),
            }),
        },
        backend,
    )
    .await;

    let spawn_state = state.clone();
    let spawn_job_id = new_job_id.clone();
    tokio::spawn(async move {
        run_backfill_job(spawn_state, spawn_job_id).await;
    });

    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": new_job_id })),
    ))
}

// ---------------------------------------------------------------------------
// Flush endpoints
// ---------------------------------------------------------------------------

pub(super) async fn flush_backfill_details(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id = ?",
        state.db_backend,
    );
    crate::db::query(&sql)
        .bind(&job_id)
        .execute(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to flush backfill details: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn flush_all_backfill_details(
    State(state): State<AppState>,
    auth: UserAuth,
) -> Result<StatusCode, AppError> {
    auth.require(Permission::BackfillCreate).await?;

    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id IN (SELECT id FROM happyview_backfill_jobs WHERE status IN ('completed', 'cancelled', 'failed'))",
        state.db_backend,
    );
    crate::db::query(&sql)
        .execute(&state.backfill_db)
        .await
        .map_err(|e| AppError::Internal(format!("failed to flush backfill details: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Retention cleanup
// ---------------------------------------------------------------------------

pub async fn run_backfill_retention_cleanup(state: &AppState) {
    use super::settings::get_setting;

    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(86400));

    loop {
        interval.tick().await;

        let retention_days: i64 = get_setting(
            &state.backfill_db,
            "backfill_retention_days",
            state.db_backend,
        )
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(28);

        if retention_days == 0 {
            continue;
        }

        let cutoff = chrono::Utc::now() - chrono::Duration::days(retention_days);
        let cutoff_str = cutoff.to_rfc3339();

        let sql = adapt_sql(
            "DELETE FROM happyview_backfill_repos WHERE job_id IN (SELECT id FROM happyview_backfill_jobs WHERE completed_at IS NOT NULL AND completed_at < ?)",
            state.db_backend,
        );
        match crate::db::query(&sql)
            .bind(&cutoff_str)
            .execute(&state.backfill_db)
            .await
        {
            Ok(result) => {
                let deleted = result.rows_affected();
                if deleted > 0 {
                    tracing::info!(
                        deleted,
                        retention_days,
                        "cleaned up old backfill detail rows"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "backfill retention cleanup failed");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Startup resumption
// ---------------------------------------------------------------------------

/// Resume any backfill jobs that were running when the server last stopped.
/// Jobs stuck in `cancelling` are finalised immediately.
pub async fn resume_backfill_jobs(state: &AppState) {
    let sql = adapt_sql(
        "SELECT id, status FROM happyview_backfill_jobs WHERE status IN ('running', 'cancelling', 'pausing')",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = crate::db::query_as(&sql)
        .fetch_all(&state.backfill_db)
        .await
        .unwrap_or_default();

    for (job_id, status) in rows {
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
                let spawn_state = state.clone();
                tokio::spawn(async move {
                    run_backfill_job(spawn_state, job_id).await;
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::{memory_pool, test_state_with_pool};
    use wiremock::matchers::{method, path, path_regex, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::lexicon::{ParsedLexicon, ProcedureAction};

    const POST: &str = "app.test.post";
    const LIKE: &str = "app.test.like";

    // -----------------------------------------------------------------------
    // Account-targeted jobs
    // -----------------------------------------------------------------------

    async fn migrated_state() -> AppState {
        test_state_with_pool(crate::test_support::migrated_memory_pool().await)
    }

    async fn seed_legacy_repo(
        state: &AppState,
        job_id: &str,
        did: &str,
        pds: Option<&str>,
        status: &str,
    ) {
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_repos (job_id, did, pds_endpoint, status) VALUES (?, ?, ?, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(job_id)
            .bind(did)
            .bind(pds)
            .bind(status)
            .execute(&state.db)
            .await
            .expect("seed legacy repo");
    }

    // -----------------------------------------------------------------------
    // Bounded queue
    // -----------------------------------------------------------------------

    #[test]
    fn the_discovery_window_defaults_and_rejects_nonsense() {
        assert_eq!(parse_discovery_window(None), DEFAULT_DISCOVERY_WINDOW);
        assert_eq!(parse_discovery_window(Some("0")), DEFAULT_DISCOVERY_WINDOW);
        assert_eq!(parse_discovery_window(Some("-5")), DEFAULT_DISCOVERY_WINDOW);
        assert_eq!(
            parse_discovery_window(Some("banana")),
            DEFAULT_DISCOVERY_WINDOW
        );
        assert_eq!(parse_discovery_window(Some("2000")), 2000);
    }

    #[test]
    fn the_window_refuses_a_reservation_that_would_overflow_and_settles() {
        let window = QueueWindow::new(5, 2);
        assert!(window.try_reserve(3));
        assert!(!window.try_reserve(1), "full");
        window.settle(3, 1);
        assert_eq!(window.queued.load(Ordering::Acquire), 3);
        window.release(3);
        assert!(window.try_reserve(5));
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_job(
        state: &AppState,
        id: &str,
        collection: Option<&str>,
        scope: &str,
        stage: &str,
        queue_version: i32,
        discovery_complete: bool,
    ) {
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_jobs \
             (id, collection, scope, status, stage, total_repos, queue_version, discovery_complete, created_at) \
             VALUES (?, ?, ?, 'running', ?, 0, ?, ?, '2026-01-01T00:00:00+00:00')",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(id)
            .bind(collection)
            .bind(scope)
            .bind(stage)
            .bind(queue_version)
            .bind(i32::from(discovery_complete))
            .execute(&state.db)
            .await
            .expect("seed job");
    }

    async fn seed_unit(
        state: &AppState,
        job_id: &str,
        collection: &str,
        did: &str,
        pds: Option<&str>,
    ) {
        let sql = adapt_sql(
            "INSERT INTO happyview_backfill_queue (job_id, collection, did, pds_endpoint) VALUES (?, ?, ?, ?)",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(job_id)
            .bind(collection)
            .bind(did)
            .bind(pds)
            .execute(&state.db)
            .await
            .expect("seed unit");
    }

    async fn count_for_job(state: &AppState, table: &str, job_id: &str) -> i64 {
        let sql = adapt_sql(
            &format!("SELECT COUNT(*) FROM {table} WHERE job_id = ?"),
            state.db_backend,
        );
        crate::db::query_as::<(i64,)>(&sql)
            .bind(job_id)
            .fetch_one(&state.db)
            .await
            .expect("count rows")
            .0
    }

    /// A job row's progress, with unset counters read as zero.
    #[derive(Debug)]
    struct JobRow {
        status: String,
        total_repos: i32,
        resolved_repos: i32,
        processed_repos: i32,
        total_records: i32,
        queue_version: i32,
        discovery_complete: i32,
    }

    async fn job_row(state: &AppState, job_id: &str) -> JobRow {
        let sql = adapt_sql(
            "SELECT status, total_repos, resolved_repos, processed_repos, total_records, queue_version, discovery_complete \
             FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        #[allow(clippy::type_complexity)]
        let (status, total, resolved, processed, records, queue_version, discovery_complete): (
            String,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            Option<i32>,
            i32,
            i32,
        ) = crate::db::query_as(&sql)
            .bind(job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
        JobRow {
            status,
            total_repos: total.unwrap_or(0),
            resolved_repos: resolved.unwrap_or(0),
            processed_repos: processed.unwrap_or(0),
            total_records: records.unwrap_or(0),
            queue_version,
            discovery_complete,
        }
    }

    fn record_lexicon(nsid: &str) -> (serde_json::Value, ParsedLexicon) {
        let lexicon = serde_json::json!({
            "lexicon": 1,
            "id": nsid,
            "defs": {"main": {"type": "record", "key": "tid"}},
        });
        let parsed = ParsedLexicon::parse(
            lexicon.clone(),
            1,
            Some(nsid.to_string()),
            ProcedureAction::Upsert,
            None,
        )
        .expect("parse lexicon");
        (lexicon, parsed)
    }

    /// A migrated state whose relay, PLC and PDS are all `mock`, with
    /// `collections` registered as record lexicons in the table and the registry.
    async fn pipeline_state(mock: &MockServer, collections: &[&str]) -> AppState {
        let mut state = migrated_state().await;
        state.config.relay_url = mock.uri();
        state.config.plc_url = mock.uri();
        for nsid in collections {
            let (lexicon, parsed) = record_lexicon(nsid);
            crate::db::query("INSERT INTO happyview_lexicons (id, lexicon_json) VALUES (?, ?)")
                .bind(*nsid)
                .bind(lexicon.to_string())
                .execute(&state.db)
                .await
                .expect("seed lexicon");
            state.lexicons.upsert(parsed).await;
        }
        state
    }

    fn did_doc(did: &str, pds: &str) -> serde_json::Value {
        serde_json::json!({
            "id": did,
            "service": [{"id": "#atproto_pds", "type": "AtprotoPersonalDataServer", "serviceEndpoint": pds}],
        })
    }

    async fn mount_relay_page(
        mock: &MockServer,
        collection: &str,
        cursor: Option<&str>,
        dids: &[&str],
        next: Option<&str>,
    ) {
        let builder = Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
            .and(query_param("collection", collection));
        let builder = match cursor {
            Some(c) => builder.and(query_param("cursor", c)),
            None => builder.and(query_param_is_missing("cursor")),
        };
        let repos: Vec<serde_json::Value> =
            dids.iter().map(|d| serde_json::json!({"did": d})).collect();
        builder
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"repos": repos, "cursor": next})),
            )
            .mount(mock)
            .await;
    }

    /// Every DID resolves to a document naming `mock` as its PDS.
    async fn mount_plc(mock: &MockServer) {
        let pds = mock.uri();
        Mock::given(method("GET"))
            .and(path_regex(r"^/did:plc:[a-z0-9]+$"))
            .respond_with(move |req: &wiremock::Request| {
                let did = req.url.path().trim_start_matches('/').to_string();
                ResponseTemplate::new(200).set_body_json(did_doc(&did, &pds))
            })
            .mount(mock)
            .await;
    }

    /// `count` records for `(did, collection)`, each with a CID that verifies,
    /// fetched exactly once.
    async fn mount_records(mock: &MockServer, did: &str, collection: &str, count: usize) {
        let records: Vec<serde_json::Value> = (0..count)
            .map(|i| {
                let value = serde_json::json!({"$type": collection, "text": format!("{did} {i}")});
                let cid = crate::cid_verify::compute_record_cid(&value)
                    .expect("cid")
                    .to_string();
                serde_json::json!({"uri": format!("at://{did}/{collection}/r{i}"), "cid": cid, "value": value})
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.listRecords"))
            .and(query_param("repo", did))
            .and(query_param("collection", collection))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"records": records})),
            )
            .expect(1)
            .mount(mock)
            .await;
    }

    /// Fails the test, when the mock server drops, if any `listRecords` call
    /// reached no other mock.
    async fn forbid_other_record_fetches(mock: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.listRecords"))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(10)
            .expect(0)
            .mount(mock)
            .await;
    }

    async fn run_to_end(state: &AppState, job_id: &str, window: i64) {
        tokio::time::timeout(
            Duration::from_secs(60),
            run_backfill_job_with(state.clone(), job_id.to_string(), window),
        )
        .await
        .expect("the backfill job should finish rather than hang");
    }

    fn bounded_queue(job_id: &str) -> JobQueue {
        JobQueue {
            job_id: Arc::new(job_id.to_string()),
            version: QueueVersion::Bounded,
            window: Arc::new(QueueWindow::new(DEFAULT_DISCOVERY_WINDOW, 0)),
            discovery_done: Arc::new(AtomicBool::new(true)),
        }
    }

    #[tokio::test]
    async fn the_resolver_reads_a_legacy_job_a_page_at_a_time() {
        let state = migrated_state().await;
        seed_job(
            &state,
            "page-job",
            None,
            "network",
            "resolving_and_fetching",
            1,
            true,
        )
        .await;
        let mut expected = Vec::new();
        for i in 0..2500 {
            let did = format!("did:plc:p{i:05}");
            let resolved = i % 10 == 0;
            seed_legacy_repo(
                &state,
                "page-job",
                &did,
                resolved.then_some("https://pds.test"),
                "pending",
            )
            .await;
            if !resolved {
                expected.push(WorkUnit::all_collections(did));
            }
        }
        assert_eq!(
            read_all_unresolved(&state, &JobQueue::legacy("page-job")).await,
            expected
        );
    }

    #[tokio::test]
    async fn the_resolver_reads_a_bounded_queue_in_collection_then_did_order() {
        let state = migrated_state().await;
        seed_job(
            &state,
            "page-bounded",
            None,
            "network",
            "resolving_and_fetching",
            2,
            true,
        )
        .await;
        let mut expected = Vec::new();
        // LIKE sorts before POST; 1200 units cross the 1000-unit page inside
        // the second collection.
        for collection in [LIKE, POST] {
            for i in 0..600 {
                let did = format!("did:plc:b{i:04}");
                let resolved = i % 7 == 0;
                seed_unit(
                    &state,
                    "page-bounded",
                    collection,
                    &did,
                    resolved.then_some("https://pds.test"),
                )
                .await;
                if !resolved {
                    expected.push(WorkUnit {
                        did,
                        collection: collection.to_string(),
                    });
                }
            }
        }
        assert_eq!(
            read_all_unresolved(&state, &bounded_queue("page-bounded")).await,
            expected
        );
    }

    async fn read_all_unresolved(state: &AppState, queue: &JobQueue) -> Vec<WorkUnit> {
        let mut seen = Vec::new();
        let mut after: Option<WorkUnit> = None;
        loop {
            let page = unresolved_page(state, queue, after.as_ref())
                .await
                .expect("page");
            assert!(page.len() as i64 <= RESOLVE_PAGE_SIZE);
            let Some(last) = page.last().cloned() else {
                return seen;
            };
            after = Some(last);
            seen.extend(page);
        }
    }

    #[tokio::test]
    async fn the_backlog_of_resolved_units_is_read_a_page_at_a_time() {
        let state = migrated_state().await;
        seed_job(
            &state,
            "backlog",
            None,
            "network",
            "resolving_and_fetching",
            2,
            true,
        )
        .await;
        let mut expected = Vec::new();
        for i in 0..1500 {
            let did = format!("did:plc:r{i:04}");
            let resolved = i % 3 != 0;
            seed_unit(
                &state,
                "backlog",
                POST,
                &did,
                resolved.then_some("https://pds.test"),
            )
            .await;
            if resolved {
                expected.push((
                    WorkUnit {
                        did,
                        collection: POST.to_string(),
                    },
                    "https://pds.test".to_string(),
                ));
            }
        }
        let queue = bounded_queue("backlog");
        let mut seen = Vec::new();
        let mut after: Option<WorkUnit> = None;
        loop {
            let page = resolved_page(&state, &queue, after.as_ref())
                .await
                .expect("page");
            assert!(page.len() as i64 <= RESOLVE_PAGE_SIZE);
            let Some((last, _)) = page.last().cloned() else {
                break;
            };
            after = Some(last);
            seen.extend(page);
        }
        assert_eq!(seen, expected);
    }

    #[tokio::test]
    async fn a_network_job_fetches_each_repo_only_for_the_collection_it_was_found_under() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST, LIKE]).await;
        mount_relay_page(&mock, POST, None, &["did:plc:a", "did:plc:b"], None).await;
        mount_relay_page(&mock, LIKE, None, &["did:plc:a"], None).await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:a", POST, 1).await;
        mount_records(&mock, "did:plc:b", POST, 2).await;
        mount_records(&mock, "did:plc:a", LIKE, 1).await;
        // `did:plc:b` was never listed under LIKE, so it is never fetched for it.
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, None, &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(job.status, "completed");
        assert_eq!(
            (
                job.total_repos,
                job.resolved_repos,
                job.processed_repos,
                job.total_records
            ),
            (3, 3, 3, 4)
        );
        assert_eq!((job.queue_version, job.discovery_complete), (2, 1));
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0,
            "completed units are deleted"
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_completions", &job_id).await,
            3
        );
        let (repos, completed, stats_records): (i64, i64, i64) = crate::db::query_as(
            "SELECT repos, completed_repos, records FROM happyview_backfill_pds_stats WHERE job_id = ? AND pds_endpoint = ?",
        )
        .bind(&job_id)
        .bind(mock.uri())
        .fetch_one(&state.db)
        .await
        .expect("pds stats");
        assert_eq!((repos, completed, stats_records), (3, 3, 4));
        let lookups = mock
            .received_requests()
            .await
            .expect("requests are recorded")
            .iter()
            .filter(|r| r.url.path() == "/did:plc:a")
            .count();
        assert_eq!(
            lookups, 1,
            "a DID queued under two collections is resolved once"
        );
    }

    #[tokio::test]
    async fn discovery_waits_for_the_window_before_the_next_relay_page() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        mount_relay_page(&mock, POST, None, &["did:plc:a", "did:plc:b"], Some("c1")).await;
        mount_relay_page(
            &mock,
            POST,
            Some("c1"),
            &["did:plc:c", "did:plc:d"],
            Some("c2"),
        )
        .await;
        mount_relay_page(&mock, POST, Some("c2"), &["did:plc:e"], None).await;
        mount_plc(&mock).await;
        for did in [
            "did:plc:a",
            "did:plc:b",
            "did:plc:c",
            "did:plc:d",
            "did:plc:e",
        ] {
            mount_records(&mock, did, POST, 1).await;
        }
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, 2).await;

        let requests = mock
            .received_requests()
            .await
            .expect("requests are recorded");
        let is_relay =
            |r: &&wiremock::Request| r.url.path() == "/xrpc/com.atproto.sync.listReposByCollection";
        assert!(
            requests
                .iter()
                .filter(is_relay)
                .all(|r| r.url.query_pairs().any(|(k, v)| k == "limit" && v == "2")),
            "a relay page must fit in the window"
        );
        let first_fetch = requests
            .iter()
            .position(|r| r.url.path() == "/xrpc/com.atproto.repo.listRecords")
            .expect("records were fetched");
        let second_page = requests
            .iter()
            .position(|r| r.url.query_pairs().any(|(k, v)| k == "cursor" && v == "c1"))
            .expect("the second relay page was requested");
        assert!(
            second_page > first_fetch,
            "the second relay page must wait until a unit from the first completes"
        );
        let job = job_row(&state, &job_id).await;
        assert_eq!(
            (job.status.as_str(), job.total_repos, job.processed_repos),
            ("completed", 5, 5)
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0
        );
    }

    #[tokio::test]
    async fn a_resumed_discovery_continues_from_its_saved_cursor() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        seed_job(
            &state,
            "resume-cursor",
            Some(POST),
            "network",
            "resolving_and_fetching",
            2,
            false,
        )
        .await;
        crate::db::query(
            "INSERT INTO happyview_backfill_cursors (job_id, collection, relay_cursor, done) VALUES ('resume-cursor', ?, 'c1', 0)",
        )
        .bind(POST)
        .execute(&state.db)
        .await
        .expect("seed cursor");
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&mock)
            .await;
        mount_relay_page(&mock, POST, Some("c1"), &["did:plc:late"], None).await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:late", POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        run_to_end(&state, "resume-cursor", DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, "resume-cursor").await;
        assert_eq!(
            (
                job.status.as_str(),
                job.processed_repos,
                job.discovery_complete
            ),
            ("completed", 1, 1)
        );
        let (done,): (i32,) = crate::db::query_as(
            "SELECT done FROM happyview_backfill_cursors WHERE job_id = 'resume-cursor'",
        )
        .fetch_one(&state.db)
        .await
        .expect("cursor row");
        assert_eq!(done, 1);
    }

    /// Review focus 1: deferred resolutions occupy the window. If only the
    /// final pass retried them, discovery would wait for room forever.
    #[tokio::test]
    async fn a_window_full_of_deferred_resolutions_still_drains() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        mount_relay_page(&mock, POST, None, &["did:plc:slow"], Some("c1")).await;
        mount_relay_page(&mock, POST, Some("c1"), &["did:plc:next"], None).await;
        // The first lookup of `slow` fails retryably, so it sits deferred in
        // a window of one while discovery waits for room.
        Mock::given(method("GET"))
            .and(path("/did:plc:slow"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&mock)
            .await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:slow", POST, 1).await;
        mount_records(&mock, "did:plc:next", POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, 1).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!((job.status.as_str(), job.processed_repos), ("completed", 2));
    }

    /// One PDS, more units than its worker's channel holds, and slow fetches.
    /// The units its worker cannot take yet wait in the dispatcher. Once the
    /// window is full nothing new arrives to prompt a retry, so the waiting
    /// units must be retried on their own or the job stalls.
    #[tokio::test]
    async fn units_waiting_on_a_busy_pds_worker_are_dispatched_without_new_arrivals() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        let dids: Vec<String> = (0..151).map(|i| format!("did:plc:u{i:04}")).collect();
        let dids: Vec<&str> = dids.iter().map(String::as_str).collect();
        mount_relay_page(&mock, POST, None, &dids[..150], Some("c1")).await;
        mount_relay_page(&mock, POST, Some("c1"), &dids[150..], None).await;
        mount_plc(&mock).await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.listRecords"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"records": []}))
                    .set_delay(Duration::from_millis(30)),
            )
            .expect(151)
            .mount(&mock)
            .await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, 150).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(
            (job.status.as_str(), job.processed_repos),
            ("completed", 151)
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0
        );
    }

    /// Review focus 2: `did:plc:a` is enqueued while the resolver is past it
    /// (on `did:plc:m`), and discovery finishes before that pass ends. The
    /// pass began before discovery finished, so another pass must follow.
    #[tokio::test]
    async fn a_unit_enqueued_behind_the_resolver_while_discovery_finishes_is_still_resolved() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        let pds = mock.uri();
        mount_relay_page(&mock, POST, None, &["did:plc:m"], Some("c1")).await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
            .and(query_param("cursor", "c1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"repos": [{"did": "did:plc:a"}]}))
                    .set_delay(Duration::from_millis(300)),
            )
            .mount(&mock)
            .await;
        Mock::given(method("GET"))
            .and(path("/did:plc:m"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(did_doc("did:plc:m", &pds))
                    .set_delay(Duration::from_millis(900)),
            )
            .with_priority(1)
            .mount(&mock)
            .await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:m", POST, 1).await;
        mount_records(&mock, "did:plc:a", POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(
            job.processed_repos, 2,
            "the unit behind the cursor must be fetched too"
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0
        );
    }

    /// A discovery that cannot write its queue fails the job; it never lets
    /// the job finish as completed on whatever had been queued.
    #[tokio::test]
    async fn a_discovery_that_cannot_write_its_queue_fails_the_job() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        crate::db::query(
            "CREATE TRIGGER refuse_cursors BEFORE INSERT ON happyview_backfill_cursors \
             BEGIN SELECT RAISE(ABORT, 'cursor writes refused'); END",
        )
        .execute(&state.db)
        .await
        .expect("create trigger");
        mount_relay_page(&mock, POST, None, &["did:plc:a"], None).await;
        mount_plc(&mock).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let (status, error): (String, Option<String>) =
            crate::db::query_as("SELECT status, error FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&state.db)
                .await
                .expect("job row");
        assert_eq!(status, "failed");
        let error = error.expect("a failed job says why");
        assert!(error.contains("cursor writes refused"), "{error}");
    }

    /// The same for a discovery that cannot read where it left off.
    #[tokio::test]
    async fn a_discovery_that_cannot_read_its_cursors_fails_the_job() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        crate::db::query("DROP TABLE happyview_backfill_cursors")
            .execute(&state.db)
            .await
            .expect("drop cursors");
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!((job.status.as_str(), job.discovery_complete), ("failed", 0));
    }

    /// A unit resolved from the DID cache sends no request, so it must leave
    /// the PLC directory's rate-limit cooldown exactly as it found it.
    #[tokio::test]
    async fn a_cache_hit_leaves_the_plc_cooldown_in_force() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST, LIKE]).await;
        let reset = chrono::Utc::now().timestamp() + 60;
        Mock::given(method("GET"))
            .and(path("/did:plc:y"))
            .respond_with(
                ResponseTemplate::new(429).insert_header("ratelimit-reset", reset.to_string()),
            )
            .mount(&mock)
            .await;
        seed_job(
            &state,
            "cooldown",
            None,
            "network",
            "resolving_and_fetching",
            2,
            true,
        )
        .await;
        for (collection, did) in [
            (POST, "did:plc:x"),
            (LIKE, "did:plc:x"),
            (POST, "did:plc:y"),
        ] {
            seed_unit(&state, "cooldown", collection, did, None).await;
        }
        let (tx, _rx) = mpsc::channel(16);
        let ctx = ResolverContext {
            state: state.clone(),
            queue: bounded_queue("cooldown"),
            resolved: Arc::new(AtomicI32::new(0)),
            cancelled: Arc::new(AtomicBool::new(false)),
            recorder: Arc::new(
                super::super::backfill_errors::ErrorRecorder::new(&state, "cooldown").await,
            ),
            concurrency: 1,
            tx,
        };
        let mut run = ResolverRun {
            cooldowns: HostCooldowns::new(),
            deferred: DeferredQueue::new(),
            deferred_units: HashSet::new(),
            recorded: HashSet::new(),
            resolved_dids: ResolvedDids::default(),
            max_attempts: 3,
            attempted: 0,
            next_cancel_check: i32::MAX,
        };
        run.resolved_dids.insert("did:plc:x", &mock.uri());
        let host = profile::did_doc_host(&state.config.plc_url, "did:plc:y");

        let limited =
            profile::resolve_pds_endpoint_once(&state.http, &state.config.plc_url, "did:plc:y")
                .await;
        let y = WorkUnit {
            did: "did:plc:y".to_string(),
            collection: POST.to_string(),
        };
        assert!(handle_resolution(&ctx, &mut run, y, limited, 1).await);
        let gate = run
            .cooldowns
            .eligible_at(&host)
            .expect("PLC is cooling down");
        assert!(gate > std::time::Instant::now() + Duration::from_secs(30));

        // A cache hit while scanning a page...
        let x_post = WorkUnit {
            did: "did:plc:x".to_string(),
            collection: POST.to_string(),
        };
        assert!(on_cached(&ctx, &mut run, x_post, mock.uri()).await);
        // ...and one while draining deferred retries.
        let x_like = WorkUnit {
            did: "did:plc:x".to_string(),
            collection: LIKE.to_string(),
        };
        let item = DeferredItem {
            payload: x_like,
            host: host.clone(),
            attempts: 1,
            eligible_at: std::time::Instant::now(),
        };
        assert!(retry_deferred_resolution(&ctx, &mut run, item).await);

        assert_eq!(run.cooldowns.eligible_at(&host), Some(gate));
        assert_eq!(run.cooldowns.consecutive_failures(&host), 1);
        assert_eq!(ctx.resolved.load(Ordering::Relaxed), 2);
    }

    /// A unit whose completion could not be committed is still queued when the
    /// run ends. The job is paused with that work left, never completed.
    #[tokio::test]
    async fn a_run_that_leaves_units_queued_pauses_rather_than_completes() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        crate::db::query(
            "CREATE TRIGGER refuse_unit_deletes BEFORE DELETE ON happyview_backfill_queue \
             BEGIN SELECT RAISE(ABORT, 'unit deletes refused'); END",
        )
        .execute(&state.db)
        .await
        .expect("create trigger");
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:stuck", POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &["did:plc:stuck".to_string()])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(job.status, "paused");
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            1
        );
        let (error,): (Option<String>,) =
            crate::db::query_as("SELECT error FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&state.db)
                .await
                .expect("job error");
        assert_eq!(
            error.as_deref(),
            Some("paused: 1 units could not be completed; resume to retry")
        );
        let (logged,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'backfill.units_left' AND subject = ?",
        )
        .bind(POST)
        .fetch_one(&state.db)
        .await
        .expect("count events");
        assert_eq!(logged, 1);
    }

    /// A relay rate limit can ask for a two-minute wait; a pause must not have
    /// to sit it out.
    #[tokio::test]
    async fn a_pause_interrupts_a_relay_rate_limit_wait() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.sync.listReposByCollection"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "120"))
            .mount(&mock)
            .await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        let pauser = {
            let state = state.clone();
            let job_id = job_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                request_pause(&state, &job_id).await;
            })
        };
        tokio::time::timeout(
            Duration::from_secs(20),
            run_backfill_job_with(state.clone(), job_id.clone(), DEFAULT_DISCOVERY_WINDOW),
        )
        .await
        .expect("the pause should not wait out the rate limit");
        pauser.await.expect("pauser");

        let job = job_row(&state, &job_id).await;
        assert_eq!((job.status.as_str(), job.discovery_complete), ("paused", 0));
    }

    #[tokio::test]
    async fn an_account_job_fetches_every_collection_for_its_account() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST, LIKE]).await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:t1", POST, 1).await;
        mount_records(&mock, "did:plc:t1", LIKE, 2).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, None, &["did:plc:t1".to_string()])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(
            (
                job.status.as_str(),
                job.total_repos,
                job.processed_repos,
                job.total_records
            ),
            ("completed", 1, 1, 3)
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0
        );
        let (collection,): (String,) = crate::db::query_as(
            "SELECT collection FROM happyview_backfill_completions WHERE job_id = ?",
        )
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("completion row");
        assert_eq!(collection, "");
    }

    #[tokio::test]
    async fn a_pre_upgrade_job_past_discovery_finishes_on_its_existing_rows() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        let pds = mock.uri();
        seed_job(
            &state,
            "legacy-fetching",
            Some(POST),
            "network",
            "resolving_and_fetching",
            1,
            true,
        )
        .await;
        crate::db::query(
            "UPDATE happyview_backfill_jobs SET total_records = 5 WHERE id = 'legacy-fetching'",
        )
        .execute(&state.db)
        .await
        .expect("seed records");
        seed_legacy_repo(&state, "legacy-fetching", "did:plc:x", None, "pending").await;
        seed_legacy_repo(
            &state,
            "legacy-fetching",
            "did:plc:y",
            Some(&pds),
            "pending",
        )
        .await;
        seed_legacy_repo(
            &state,
            "legacy-fetching",
            "did:plc:z",
            Some(&pds),
            "completed",
        )
        .await;
        mount_plc(&mock).await;
        mount_records(&mock, "did:plc:x", POST, 1).await;
        mount_records(&mock, "did:plc:y", POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        run_to_end(&state, "legacy-fetching", DEFAULT_DISCOVERY_WINDOW).await;

        let rows: Vec<(String, String, i32)> = crate::db::query_as(
            "SELECT did, status, records_fetched FROM happyview_backfill_repos WHERE job_id = 'legacy-fetching' ORDER BY did",
        )
        .fetch_all(&state.db)
        .await
        .expect("legacy rows");
        assert_eq!(
            rows,
            vec![
                ("did:plc:x".to_string(), "completed".to_string(), 1),
                ("did:plc:y".to_string(), "completed".to_string(), 1),
                ("did:plc:z".to_string(), "completed".to_string(), 0),
            ]
        );
        let job = job_row(&state, "legacy-fetching").await;
        assert_eq!(
            (
                job.status.as_str(),
                job.resolved_repos,
                job.processed_repos,
                job.total_records,
                job.queue_version
            ),
            ("completed", 3, 3, 7, 1)
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", "legacy-fetching").await,
            0
        );
    }

    /// Several repos on one PDS fetch concurrently. Each fetch writes its
    /// pages in transactions, so a fetch the job stopped polling mid-page
    /// would hold the (here, only) connection while the job waited on it.
    #[tokio::test]
    async fn a_pre_upgrade_job_in_the_fetching_stage_finishes_its_rows() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        let pds = mock.uri();
        seed_job(
            &state,
            "legacy-fetch-stage",
            Some(POST),
            "network",
            "fetching_records",
            1,
            true,
        )
        .await;
        for did in ["did:plc:f1", "did:plc:f2", "did:plc:f3"] {
            seed_legacy_repo(&state, "legacy-fetch-stage", did, Some(&pds), "pending").await;
            mount_records(&mock, did, POST, 2).await;
        }
        forbid_other_record_fetches(&mock).await;

        run_to_end(&state, "legacy-fetch-stage", DEFAULT_DISCOVERY_WINDOW).await;

        let job = job_row(&state, "legacy-fetch-stage").await;
        assert_eq!(
            (job.status.as_str(), job.processed_repos, job.total_records),
            ("completed", 3, 6)
        );
        let (completed,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = 'legacy-fetch-stage' AND status = 'completed'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count completed");
        assert_eq!(completed, 3);
    }

    async fn assert_a_discovering_legacy_job_restarts(
        state: &AppState,
        mock: &MockServer,
        job_id: &str,
        [p, q, r]: [&str; 3],
    ) {
        seed_job(
            state,
            job_id,
            Some(POST),
            "network",
            "discovering_repos",
            1,
            true,
        )
        .await;
        seed_legacy_repo(state, job_id, p, None, "pending").await;
        seed_legacy_repo(state, job_id, q, None, "pending").await;
        mount_relay_page(mock, POST, None, &[q, r], None).await;
        mount_plc(mock).await;
        mount_records(mock, q, POST, 1).await;
        mount_records(mock, r, POST, 1).await;
        // `p` was only in the discarded partial list.
        forbid_other_record_fetches(mock).await;

        run_to_end(state, job_id, DEFAULT_DISCOVERY_WINDOW).await;

        assert_eq!(
            count_for_job(state, "happyview_backfill_repos", job_id).await,
            0
        );
        let job = job_row(state, job_id).await;
        assert_eq!(
            (
                job.status.as_str(),
                job.total_repos,
                job.processed_repos,
                job.queue_version,
                job.discovery_complete
            ),
            ("completed", 2, 2, 2, 1)
        );
        assert_eq!(
            count_for_job(state, "happyview_backfill_queue", job_id).await,
            0
        );
    }

    #[tokio::test]
    async fn a_pre_upgrade_job_still_discovering_restarts_on_the_bounded_queue() {
        let mock = MockServer::start().await;
        let state = pipeline_state(&mock, &[POST]).await;
        assert_a_discovering_legacy_job_restarts(
            &state,
            &mock,
            "legacy-discovering",
            ["did:plc:p", "did:plc:q", "did:plc:r"],
        )
        .await;
    }

    /// Completes `RECENT_COMPLETIONS_PER_JOB + COMPLETIONS_TRIM_EVERY + 5`
    /// units. Completing a unit never trims (R10); the scheduled trim runs on
    /// the job's every `COMPLETIONS_TRIM_EVERY`th completion only.
    async fn assert_the_completion_log_is_trimmed(state: &AppState, job_id: &str) {
        seed_job(
            state,
            job_id,
            Some(POST),
            "network",
            "resolving_and_fetching",
            2,
            true,
        )
        .await;
        let queue = bounded_queue(job_id);
        let total = RECENT_COMPLETIONS_PER_JOB + i64::from(COMPLETIONS_TRIM_EVERY) + 5;
        for i in 0..total {
            let unit = WorkUnit {
                did: format!("did:plc:{i:05}"),
                collection: POST.to_string(),
            };
            complete_unit(state, &queue, &unit, "https://pds.test", 1)
                .await
                .expect("complete unit");
        }
        assert_eq!(
            count_for_job(state, "happyview_backfill_completions", job_id).await,
            total,
            "completing a unit does not trim"
        );

        trim_completions_on_schedule(state, &queue, COMPLETIONS_TRIM_EVERY + 1).await;
        assert_eq!(
            count_for_job(state, "happyview_backfill_completions", job_id).await,
            total,
            "off schedule, nothing is trimmed"
        );

        trim_completions_on_schedule(state, &queue, COMPLETIONS_TRIM_EVERY * 11).await;
        assert_eq!(
            count_for_job(state, "happyview_backfill_completions", job_id).await,
            RECENT_COMPLETIONS_PER_JOB
        );
        let sql = adapt_sql(
            "SELECT did FROM happyview_backfill_completions WHERE job_id = ? ORDER BY id LIMIT 1",
            state.db_backend,
        );
        let (oldest,): (String,) = crate::db::query_as(&sql)
            .bind(job_id)
            .fetch_one(&state.db)
            .await
            .expect("oldest kept");
        assert_eq!(
            oldest,
            format!("did:plc:{:05}", total - RECENT_COMPLETIONS_PER_JOB)
        );
        let job = job_row(state, job_id).await;
        assert_eq!(
            (i64::from(job.processed_repos), i64::from(job.total_records)),
            (total, total)
        );
    }

    #[tokio::test]
    async fn the_completion_log_keeps_only_the_most_recent_units() {
        let state = migrated_state().await;
        assert_the_completion_log_is_trimmed(&state, "log-job").await;
    }

    // -----------------------------------------------------------------------
    // Bounded queue on Postgres
    // -----------------------------------------------------------------------

    /// A Postgres state whose relay, PLC and PDS are `mock`, with POST in the
    /// registry. Jobs name their collection, so no lexicon row is needed.
    async fn postgres_pipeline_state(mock: &MockServer) -> Option<AppState> {
        let mut state = crate::test_support::test_state_from_env().await?;
        state.config.relay_url = mock.uri();
        state.config.plc_url = mock.uri();
        state.lexicons.upsert(record_lexicon(POST).1).await;
        Some(state)
    }

    async fn delete_postgres_job(state: &AppState, job_id: &str, dids: &[&str]) {
        for did in dids {
            let sql = adapt_sql(
                "DELETE FROM happyview_records WHERE did = ?",
                state.db_backend,
            );
            crate::db::query(&sql)
                .bind(*did)
                .execute(&state.db)
                .await
                .expect("delete records");
        }
        let sql = adapt_sql(
            "DELETE FROM happyview_backfill_jobs WHERE id = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(job_id)
            .execute(&state.db)
            .await
            .expect("delete job");
    }

    #[tokio::test]
    async fn a_network_job_runs_on_the_bounded_queue_on_postgres() {
        let mock = MockServer::start().await;
        let Some(state) = postgres_pipeline_state(&mock).await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        let tag = Uuid::new_v4().simple().to_string();
        let a = format!("did:plc:a{tag}");
        let b = format!("did:plc:b{tag}");
        let c = format!("did:plc:c{tag}");
        mount_relay_page(&mock, POST, None, &[&a, &b], Some("c1")).await;
        mount_relay_page(&mock, POST, Some("c1"), &[&c], None).await;
        mount_plc(&mock).await;
        mount_records(&mock, &a, POST, 1).await;
        mount_records(&mock, &b, POST, 2).await;
        mount_records(&mock, &c, POST, 1).await;
        forbid_other_record_fetches(&mock).await;

        let job_id = create_backfill_job(&state, Some(POST), &[])
            .await
            .expect("create job");
        run_to_end(&state, &job_id, 2).await;

        let job = job_row(&state, &job_id).await;
        assert_eq!(
            (
                job.status.as_str(),
                job.total_repos,
                job.resolved_repos,
                job.processed_repos,
                job.total_records,
                job.discovery_complete
            ),
            ("completed", 3, 3, 3, 4, 1)
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_queue", &job_id).await,
            0
        );
        assert_eq!(
            count_for_job(&state, "happyview_backfill_completions", &job_id).await,
            3
        );
        let sql = adapt_sql(
            "SELECT repos, completed_repos, records FROM happyview_backfill_pds_stats WHERE job_id = ?",
            state.db_backend,
        );
        let stats: (i64, i64, i64) = crate::db::query_as(&sql)
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("pds stats");
        assert_eq!(stats, (3, 3, 4));
        let sql = adapt_sql(
            "SELECT relay_cursor, done FROM happyview_backfill_cursors WHERE job_id = ?",
            state.db_backend,
        );
        let cursor: (Option<String>, i32) = crate::db::query_as(&sql)
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("cursor row");
        assert_eq!(cursor, (None, 1));

        delete_postgres_job(&state, &job_id, &[&a, &b, &c]).await;
    }

    #[tokio::test]
    async fn a_pre_upgrade_job_still_discovering_restarts_on_postgres() {
        let mock = MockServer::start().await;
        let Some(state) = postgres_pipeline_state(&mock).await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        let tag = Uuid::new_v4().simple().to_string();
        let job_id = format!("legacy-{tag}");
        let dids = [
            format!("did:plc:p{tag}"),
            format!("did:plc:q{tag}"),
            format!("did:plc:r{tag}"),
        ];
        let [p, q, r] = [&dids[0], &dids[1], &dids[2]].map(String::as_str);
        assert_a_discovering_legacy_job_restarts(&state, &mock, &job_id, [p, q, r]).await;
        delete_postgres_job(&state, &job_id, &[p, q, r]).await;
    }

    #[tokio::test]
    async fn the_completion_log_is_trimmed_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        let job_id = format!("log-{}", Uuid::new_v4().simple());
        assert_the_completion_log_is_trimmed(&state, &job_id).await;
        delete_postgres_job(&state, &job_id, &[]).await;
    }

    /// Resolves `did:` entries to themselves and `<name>.test` handles to
    /// `did:plc:<name>`; anything else fails.
    fn fake_resolve(input: String) -> std::future::Ready<Result<String, AppError>> {
        let result = if input.starts_with("did:") {
            Ok(input)
        } else if let Some(name) = input.trim_start_matches('@').strip_suffix(".test") {
            Ok(format!("did:plc:{name}"))
        } else {
            Err(AppError::BadRequest(format!(
                "could not resolve handle {input}"
            )))
        };
        std::future::ready(result)
    }

    #[tokio::test]
    async fn accounts_are_resolved_and_deduplicated_in_order() {
        let dids = resolve_backfill_accounts(
            vec![
                " did:plc:bob ".into(),
                "alice.test".into(),
                "did:plc:alice".into(),
                "did:plc:bob".into(),
                "".into(),
            ],
            fake_resolve,
        )
        .await
        .expect("resolve accounts");

        assert_eq!(
            dids,
            vec!["did:plc:bob".to_string(), "did:plc:alice".to_string()]
        );
    }

    #[tokio::test]
    async fn blank_only_dids_are_refused() {
        let result = resolve_backfill_accounts(vec!["  ".into(), "".into()], fake_resolve).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "got {result:?}"
        );
    }

    /// `create_backfill` appends `did` to the `dids` list, so a blank `did`
    /// on its own arrives as a single blank entry.
    #[tokio::test]
    async fn a_blank_did_alone_is_refused() {
        let result = resolve_backfill_accounts(vec!["".into()], fake_resolve).await;
        assert!(
            matches!(result, Err(AppError::BadRequest(_))),
            "got {result:?}"
        );
    }

    #[tokio::test]
    async fn no_accounts_resolves_to_no_dids() {
        let dids = resolve_backfill_accounts(Vec::new(), fake_resolve)
            .await
            .expect("empty list is a network backfill");
        assert!(dids.is_empty());
    }

    #[tokio::test]
    async fn any_unresolvable_account_fails_the_request_naming_each_one() {
        let err = resolve_backfill_accounts(
            vec![
                "alice.test".into(),
                "nope.example".into(),
                "also-bad.example".into(),
            ],
            fake_resolve,
        )
        .await
        .expect_err("should refuse");

        let AppError::BadRequest(msg) = err else {
            panic!("expected BadRequest, got {err:?}");
        };
        assert!(msg.contains("nope.example"), "{msg}");
        assert!(msg.contains("also-bad.example"), "{msg}");
    }

    #[tokio::test]
    async fn more_than_the_account_cap_is_refused_before_resolving() {
        let inputs: Vec<String> = (0..=MAX_BACKFILL_ACCOUNTS)
            .map(|i| format!("did:plc:a{i}"))
            .collect();
        let calls = std::sync::atomic::AtomicUsize::new(0);

        let result = resolve_backfill_accounts(inputs, |input| {
            calls.fetch_add(1, Ordering::Relaxed);
            fake_resolve(input)
        })
        .await;

        assert!(matches!(result, Err(AppError::BadRequest(_))));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn a_multi_account_job_is_seeded_and_skips_discovery() {
        let state = migrated_state().await;
        let dids = vec!["did:plc:a".to_string(), "did:plc:b".to_string()];

        let job_id = create_backfill_job(&state, Some("dummy.collection"), &dids)
            .await
            .expect("create job");

        let (did, scope, stage, total): (Option<String>, String, String, Option<i32>) =
            crate::db::query_as(
                "SELECT did, scope, stage, total_repos FROM happyview_backfill_jobs WHERE id = ?",
            )
            .bind(&job_id)
            .fetch_one(&state.db)
            .await
            .expect("job row");
        assert_eq!(did, None);
        assert_eq!(scope, "dids");
        assert_eq!(stage, "resolving_pds");
        assert_eq!(total, Some(2));

        let repos: Vec<(String,)> = crate::db::query_as(
            "SELECT did FROM happyview_backfill_queue WHERE job_id = ? AND collection = '' ORDER BY did",
        )
        .bind(&job_id)
        .fetch_all(&state.db)
        .await
        .expect("repo rows");
        assert_eq!(repos.into_iter().map(|(d,)| d).collect::<Vec<_>>(), dids);
    }

    #[tokio::test]
    async fn a_single_account_job_records_its_did() {
        let state = migrated_state().await;

        let job_id = create_backfill_job(&state, None, &["did:plc:solo".to_string()])
            .await
            .expect("create job");

        let (did, scope): (Option<String>, String) =
            crate::db::query_as("SELECT did, scope FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&state.db)
                .await
                .expect("job row");
        assert_eq!(did.as_deref(), Some("did:plc:solo"));
        assert_eq!(scope, "dids");
    }

    #[tokio::test]
    async fn a_job_without_accounts_discovers_across_the_network() {
        let state = migrated_state().await;

        let job_id = create_backfill_job(&state, Some("dummy.collection"), &[])
            .await
            .expect("create job");

        let (did, scope, stage, queue_version, discovery_complete): (
            Option<String>,
            String,
            String,
            i32,
            i32,
        ) = crate::db::query_as(
            "SELECT did, scope, stage, queue_version, discovery_complete FROM happyview_backfill_jobs WHERE id = ?",
        )
        .bind(&job_id)
        .fetch_one(&state.db)
        .await
        .expect("job row");
        assert_eq!(did, None);
        assert_eq!(scope, "network");
        assert_eq!(stage, "pending");
        assert_eq!((queue_version, discovery_complete), (2, 0));
    }

    #[tokio::test]
    async fn the_legacy_did_field_is_merged_into_dids() {
        let state = migrated_state().await;

        let (status, Json(body)) = create_backfill(
            State(state.clone()),
            super_auth(&state),
            Json(CreateBackfillBody {
                collection: None,
                did: Some("did:plc:legacy".into()),
                dids: Some(vec!["did:plc:other".into()]),
            }),
        )
        .await
        .expect("create backfill");
        assert_eq!(status, StatusCode::CREATED);
        let job_id = body["id"].as_str().expect("id").to_string();

        let (scope,): (String,) =
            crate::db::query_as("SELECT scope FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&state.db)
                .await
                .expect("job row");
        assert_eq!(scope, "dids");

        let repos: Vec<(String,)> = crate::db::query_as(
            "SELECT did FROM happyview_backfill_queue WHERE job_id = ? AND collection = '' ORDER BY did",
        )
        .bind(&job_id)
        .fetch_all(&state.db)
        .await
        .expect("repo rows");
        assert_eq!(
            repos.into_iter().map(|(d,)| d).collect::<Vec<_>>(),
            vec!["did:plc:legacy".to_string(), "did:plc:other".to_string()]
        );
    }

    async fn state_with_job(
        job_id: &str,
        status: &str,
        stage: &str,
        error_counts: Option<&str>,
    ) -> AppState {
        let pool = memory_pool().await;
        crate::db::query(
            "CREATE TABLE happyview_backfill_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                stage TEXT NOT NULL,
                total_repos INTEGER,
                resolved_repos INTEGER,
                processed_repos INTEGER,
                total_records INTEGER,
                error_counts TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("create happyview_backfill_jobs table: {e}"));

        crate::db::query(
            "INSERT INTO happyview_backfill_jobs \
             (id, status, stage, total_repos, resolved_repos, processed_repos, total_records, error_counts) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job_id)
        .bind(status)
        .bind(stage)
        .bind(10i32)
        .bind(4i32)
        .bind(2i32)
        .bind(50i32)
        .bind(error_counts)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert backfill job: {e}"));

        test_state_with_pool(pool)
    }

    #[tokio::test]
    async fn snapshot_reflects_current_row() {
        let state = state_with_job(
            "job-1",
            "running",
            "resolving_and_fetching",
            Some(r#"{"dns_failure":2}"#),
        )
        .await;

        let event = build_job_snapshot(&state, "job-1")
            .await
            .expect("snapshot for existing job");

        match event {
            super::super::types::BackfillEvent::JobSnapshot {
                job_id,
                status,
                stage,
                total_repos,
                resolved_repos,
                processed_repos,
                total_records,
                error_counts,
            } => {
                assert_eq!(job_id, "job-1");
                assert_eq!(status, "running");
                assert_eq!(stage, "resolving_and_fetching");
                assert_eq!(total_repos, Some(10));
                assert_eq!(resolved_repos, Some(4));
                assert_eq!(processed_repos, Some(2));
                assert_eq!(total_records, Some(50));
                assert_eq!(error_counts, serde_json::json!({"dns_failure": 2}));
            }
            other => panic!("expected JobSnapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_defaults_error_counts_when_null() {
        let state = state_with_job("job-2", "completed", "completed", None).await;

        let event = build_job_snapshot(&state, "job-2")
            .await
            .expect("snapshot for existing job");

        match event {
            super::super::types::BackfillEvent::JobSnapshot { error_counts, .. } => {
                assert_eq!(error_counts, serde_json::json!({}));
            }
            other => panic!("expected JobSnapshot, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn snapshot_is_none_for_unknown_job() {
        let state = state_with_job("job-3", "running", "discovering_repos", None).await;

        assert!(build_job_snapshot(&state, "does-not-exist").await.is_none());
    }

    // -----------------------------------------------------------------------
    // Errors API
    // -----------------------------------------------------------------------

    async fn state_with_errors_job(
        job_id: &str,
        collection: Option<&str>,
        error_counts_json: &str,
    ) -> AppState {
        let state = migrated_state().await;
        crate::db::query(
            "INSERT INTO happyview_backfill_jobs \
             (id, collection, did, status, stage, created_at, error_counts) \
             VALUES (?, ?, NULL, 'completed', 'completed', '2026-01-01T00:00:00+00:00', ?)",
        )
        .bind(job_id)
        .bind(collection)
        .bind(error_counts_json)
        .execute(&state.db)
        .await
        .unwrap_or_else(|e| panic!("insert backfill job: {e}"));
        state
    }

    fn super_auth(state: &AppState) -> UserAuth {
        UserAuth {
            did: "did:plc:admin".to_string(),
            user_id: "admin".to_string(),
            is_super: true,
            is_platform: false,
            permissions: std::collections::HashSet::new(),
            db: state.db.clone(),
            db_backend: state.db_backend,
        }
    }

    async fn insert_error(
        state: &AppState,
        job_id: &str,
        did: &str,
        phase: &str,
        kind: BackfillErrorKind,
    ) {
        crate::db::query(
            "INSERT INTO happyview_backfill_errors \
             (job_id, did, collection, phase, kind, message, attempts, last_at) \
             VALUES (?, ?, NULL, ?, ?, 'boom', 1, '2026-01-01T00:00:00+00:00')",
        )
        .bind(job_id)
        .bind(did)
        .bind(phase)
        .bind(kind.as_str())
        .execute(&state.backfill_db)
        .await
        .unwrap_or_else(|e| panic!("insert backfill error: {e}"));
    }

    #[tokio::test]
    async fn errors_list_reports_exact_counts_and_capped_flag() {
        let state = state_with_errors_job(
            "job-err-1",
            Some("dummy.collection"),
            r#"{"dns_failure":2,"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:b",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-err-1",
            "did:plc:c",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-1".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert_eq!(response.errors.len(), 3);
        assert!(!response.capped);

        let dns = response
            .counts
            .iter()
            .find(|c| c.kind == "dns_failure")
            .expect("dns_failure present");
        assert_eq!(dns.count, 2);
        assert!(dns.retryable);

        let repo = response
            .counts
            .iter()
            .find(|c| c.kind == "repo_not_found")
            .expect("repo_not_found present");
        assert_eq!(repo.count, 1);
        assert!(!repo.retryable);

        // Zero-count kinds are skipped rather than zero-filled.
        assert!(!response.counts.iter().any(|c| c.kind == "timeout"));
    }

    #[tokio::test]
    async fn errors_list_counts_are_live_before_the_first_flush() {
        // `error_counts` is only written by a terminal `ErrorRecorder::flush`,
        // so mid-run it is empty (or, after a crash, stale) while detail rows
        // pile up underneath. Counting the table is what puts an Errors row on
        // the dashboard during the hours a backfill actually takes.
        let state = state_with_errors_job("job-err-live", None, "{}").await;
        for (did, kind) in [
            ("did:plc:a", BackfillErrorKind::DnsFailure),
            ("did:plc:b", BackfillErrorKind::DnsFailure),
            ("did:plc:c", BackfillErrorKind::RepoNotFound),
        ] {
            insert_error(&state, "job-err-live", did, "resolve", kind).await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-live".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        let dns = response
            .counts
            .iter()
            .find(|c| c.kind == "dns_failure")
            .expect("dns_failure present despite an empty error_counts");
        assert_eq!(dns.count, 2);
        assert_eq!(
            response
                .counts
                .iter()
                .find(|c| c.kind == "repo_not_found")
                .map(|c| c.count),
            Some(1)
        );
    }

    #[tokio::test]
    async fn errors_list_counts_keep_the_json_where_it_exceeds_the_table() {
        // Above `ERROR_DETAIL_CAP` rows stop being written, so the flushed JSON
        // is the only remaining record of how many failures there were. The
        // table must raise a count, never lower one.
        let state =
            state_with_errors_job("job-err-max", None, r#"{"dns_failure":9000,"timeout":1}"#).await;
        insert_error(
            &state,
            "job-err-max",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        for did in ["did:plc:b", "did:plc:c"] {
            insert_error(
                &state,
                "job-err-max",
                did,
                "resolve",
                BackfillErrorKind::Timeout,
            )
            .await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-max".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        let count = |kind: &str| {
            response
                .counts
                .iter()
                .find(|c| c.kind == kind)
                .map(|c| c.count)
        };
        // JSON wins where it is higher...
        assert_eq!(count("dns_failure"), Some(9000));
        // ...and the table wins where it is.
        assert_eq!(count("timeout"), Some(2));
    }

    #[test]
    fn a_retryable_give_up_is_recorded_in_preference_to_a_permanent_one() {
        // Only one give-up per DID reaches the recorder; `retry-failed`
        // selects on retryability, so the retryable one is what keeps the DID
        // reachable by a retry job.
        use crate::admin::backfill_errors::BackfillFailure;

        let failed = |kind: BackfillErrorKind| FetchOutcome::Failed {
            count: 0,
            cursor: None,
            failure: BackfillFailure {
                kind,
                message: String::new(),
                retry_after: None,
            },
        };

        let mut failures = vec![
            (
                "app.bsky.feed.post".to_string(),
                failed(BackfillErrorKind::RepoNotFound),
            ),
            (
                "app.bsky.feed.like".to_string(),
                failed(BackfillErrorKind::Other),
            ),
            (
                "app.bsky.graph.follow".to_string(),
                failed(BackfillErrorKind::Timeout),
            ),
        ];
        retryable_give_ups_first(&mut failures);

        assert_eq!(failures[0].0, "app.bsky.graph.follow");
        // The rest keep their original relative order, so the log stays
        // predictable.
        assert_eq!(failures[1].0, "app.bsky.feed.post");
        assert_eq!(failures[2].0, "app.bsky.feed.like");
    }

    #[tokio::test]
    async fn errors_list_reports_capped_once_the_total_reaches_the_cap() {
        let state = state_with_errors_job(
            "job-err-cap",
            None,
            &format!(r#"{{"dns_failure":{ERROR_DETAIL_CAP}}}"#),
        )
        .await;

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-cap".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: None,
                cursor: None,
                limit: None,
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert!(response.capped);
        assert_eq!(response.cap, ERROR_DETAIL_CAP);
    }

    #[tokio::test]
    async fn errors_list_filters_by_kind_and_paginates_by_cursor() {
        let state = state_with_errors_job("job-err-2", None, "{}").await;
        for (did, kind) in [
            ("did:plc:a", BackfillErrorKind::DnsFailure),
            ("did:plc:b", BackfillErrorKind::DnsFailure),
            ("did:plc:c", BackfillErrorKind::RepoNotFound),
        ] {
            insert_error(&state, "job-err-2", did, "resolve", kind).await;
        }

        let response = backfill_errors_list(
            State(state.clone()),
            Path("job-err-2".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: Some("dns_failure".to_string()),
                cursor: None,
                limit: Some(1),
            }),
        )
        .await
        .expect("list errors")
        .0;

        assert_eq!(response.errors.len(), 1);
        assert_eq!(response.errors[0].did, "did:plc:a");
        assert_eq!(response.errors[0].kind, "dns_failure");
        assert_eq!(response.cursor.as_deref(), Some("did:plc:a"));

        // Turn the page: the cursor from page 1 must reach the remaining
        // dns_failure row (did:plc:b) and not did:plc:c, which is filtered
        // out by kind, and the second page must terminate the pagination.
        let page2 = backfill_errors_list(
            State(state.clone()),
            Path("job-err-2".to_string()),
            super_auth(&state),
            axum::extract::Query(BackfillErrorsQuery {
                kind: Some("dns_failure".to_string()),
                cursor: response.cursor.clone(),
                limit: Some(1),
            }),
        )
        .await
        .expect("list errors page 2")
        .0;

        assert_eq!(page2.errors.len(), 1);
        assert_eq!(page2.errors[0].did, "did:plc:b");
        assert_eq!(page2.errors[0].kind, "dns_failure");
        assert_eq!(page2.cursor, None, "second page should end the pagination");
    }

    #[tokio::test]
    async fn retry_failed_seeds_new_job_from_retryable_kinds_only() {
        let state = state_with_errors_job(
            "job-retry-1",
            Some("dummy.collection"),
            r#"{"dns_failure":2,"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:a",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:b",
            "resolve",
            BackfillErrorKind::DnsFailure,
        )
        .await;
        insert_error(
            &state,
            "job-retry-1",
            "did:plc:c",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let (status, Json(body)) = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-1".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect("retry-failed");

        assert_eq!(status, StatusCode::CREATED);
        let new_job_id = body["id"].as_str().expect("id field").to_string();
        assert_ne!(new_job_id, "job-retry-1");

        let (stage, scope): (String, String) =
            crate::db::query_as("SELECT stage, scope FROM happyview_backfill_jobs WHERE id = ?")
                .bind(&new_job_id)
                .fetch_one(&state.backfill_db)
                .await
                .expect("new job row");
        assert_eq!(stage, "resolving_pds");
        assert_eq!(scope, "dids");

        let mut repo_dids: Vec<String> = crate::db::query_as::<(String,)>(
            "SELECT did FROM happyview_backfill_queue WHERE job_id = ? ORDER BY did",
        )
        .bind(&new_job_id)
        .fetch_all(&state.backfill_db)
        .await
        .expect("repo rows")
        .into_iter()
        .map(|(did,)| did)
        .collect();
        repo_dids.sort();
        assert_eq!(
            repo_dids,
            vec!["did:plc:a".to_string(), "did:plc:b".to_string()],
            "only the retryable dns_failure DIDs should be re-seeded"
        );
    }

    #[tokio::test]
    async fn retry_failed_honors_explicit_kinds_beyond_the_default_retryable_set() {
        let state = state_with_errors_job(
            "job-retry-2",
            Some("dummy.collection"),
            r#"{"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-2",
            "did:plc:a",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let (status, Json(body)) = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-2".to_string()),
            Json(RetryFailedBody {
                kinds: Some(vec!["repo_not_found".to_string()]),
            }),
        )
        .await
        .expect("retry-failed with an explicit non-retryable kind");

        assert_eq!(status, StatusCode::CREATED);
        let new_job_id = body["id"].as_str().expect("id field").to_string();
        let (did,): (String,) =
            crate::db::query_as("SELECT did FROM happyview_backfill_queue WHERE job_id = ?")
                .bind(&new_job_id)
                .fetch_one(&state.backfill_db)
                .await
                .expect("repo row");
        assert_eq!(did, "did:plc:a");
    }

    #[tokio::test]
    async fn retry_failed_rejects_when_nothing_retryable() {
        let state = state_with_errors_job(
            "job-retry-3",
            Some("dummy.collection"),
            r#"{"repo_not_found":1}"#,
        )
        .await;
        insert_error(
            &state,
            "job-retry-3",
            "did:plc:a",
            "fetch",
            BackfillErrorKind::RepoNotFound,
        )
        .await;

        let err = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("job-retry-3".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect_err("should reject a job with no retryable failures");

        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn retry_failed_returns_not_found_for_unknown_job() {
        let state = state_with_errors_job("job-retry-4", None, "{}").await;

        let err = retry_failed_backfill(
            State(state.clone()),
            super_auth(&state),
            Path("does-not-exist".to_string()),
            Json(RetryFailedBody { kinds: None }),
        )
        .await
        .expect_err("should 404 for an unknown job");

        assert!(matches!(err, AppError::NotFound(_)));
    }

    // -----------------------------------------------------------------------
    // Page writes
    // -----------------------------------------------------------------------

    fn prepared(rkey: &str, body: serde_json::Value) -> PreparedRecord {
        PreparedRecord {
            uri: format!("at://did:plc:page/{POST}/{rkey}"),
            did: "did:plc:page".to_string(),
            collection: POST.to_string(),
            rkey: rkey.to_string(),
            record_json: body.to_string(),
            cid: format!("bafy{rkey}"),
        }
    }

    async fn ref_count(state: &AppState, uri: &str) -> i64 {
        let sql = adapt_sql(
            "SELECT COUNT(*) FROM happyview_record_refs WHERE source_uri = ?",
            state.db_backend,
        );
        crate::db::query_as::<(i64,)>(&sql)
            .bind(uri)
            .fetch_one(&state.db)
            .await
            .expect("count refs")
            .0
    }

    /// An identical page writes nothing and leaves refs alone (they are
    /// deleted here so a rewrite would show); an edited row is written and
    /// its refs rebuilt.
    async fn assert_page_writes_only_changes(state: &AppState, prefix: &str) {
        let subject = "at://did:plc:t/app.test.post/1";
        let page = vec![
            prepared(
                &format!("{prefix}a"),
                serde_json::json!({"subject": subject}),
            ),
            prepared(&format!("{prefix}b"), serde_json::json!({"text": "plain"})),
        ];
        let linked = page[0].uri.clone();

        let written = write_records_page(state, &page).await.expect("first write");
        assert_eq!(written.len(), 2);
        assert_eq!(ref_count(state, &linked).await, 1);

        let sql = adapt_sql(
            "DELETE FROM happyview_record_refs WHERE source_uri = ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(&linked)
            .execute(&state.db)
            .await
            .expect("delete refs");
        let again = write_records_page(state, &page)
            .await
            .expect("identical write");
        assert!(
            again.is_empty(),
            "an identical page writes nothing: {again:?}"
        );
        assert_eq!(
            ref_count(state, &linked).await,
            0,
            "unchanged rows keep their refs untouched"
        );

        let mut edited = page;
        edited[0].record_json =
            serde_json::json!({"subject": subject, "text": "edited"}).to_string();
        let changed = write_records_page(state, &edited)
            .await
            .expect("edited write");
        assert_eq!(changed, vec![linked.clone()]);
        assert_eq!(
            ref_count(state, &linked).await,
            1,
            "a changed row gets its refs rebuilt"
        );
    }

    #[tokio::test]
    async fn a_backfill_page_skips_unchanged_rows() {
        let state = migrated_state().await;
        assert_page_writes_only_changes(&state, "s").await;
    }

    #[tokio::test]
    async fn a_backfill_page_skips_unchanged_rows_on_postgres() {
        let Some(state) = crate::test_support::test_state_from_env().await else {
            eprintln!("skipped (TEST_DATABASE_URL not set)");
            return;
        };
        let prefix = format!("p{}", Uuid::new_v4().simple());
        assert_page_writes_only_changes(&state, &prefix).await;

        let sql = adapt_sql(
            "DELETE FROM happyview_records WHERE uri LIKE ?",
            state.db_backend,
        );
        crate::db::query(&sql)
            .bind(format!("at://did:plc:page/{POST}/{prefix}%"))
            .execute(&state.db)
            .await
            .expect("clean up");
    }

    /// A page that cannot be written rolls back whole and becomes a fetch
    /// failure the recorder keeps, rather than a warning nobody reads.
    #[tokio::test]
    async fn a_page_that_cannot_be_written_is_a_fetch_failure() {
        let mock = MockServer::start().await;
        let state = migrated_state().await;
        crate::db::query("DROP TABLE happyview_record_refs")
            .execute(&state.db)
            .await
            .expect("drop refs table");
        let value = serde_json::json!({"$type": POST, "subject": "at://did:plc:t/app.test.post/1"});
        let cid = crate::cid_verify::compute_record_cid(&value)
            .expect("cid")
            .to_string();
        Mock::given(method("GET"))
            .and(path("/xrpc/com.atproto.repo.listRecords"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "records": [{"uri": format!("at://did:plc:f/{POST}/1"), "cid": cid, "value": value}]
            })))
            .mount(&mock)
            .await;

        let outcome = fetch_records_page_loop(
            &state,
            &mock.uri(),
            "did:plc:f",
            POST,
            None,
            &AtomicBool::new(false),
        )
        .await;

        match outcome {
            FetchOutcome::Failed {
                count,
                cursor,
                failure,
            } => {
                assert_eq!(failure.kind, BackfillErrorKind::Other);
                assert!(
                    failure.message.contains("database write failed"),
                    "{}",
                    failure.message
                );
                assert_eq!((count, cursor), (0, None));
            }
            FetchOutcome::Complete { .. } => {
                panic!("a page that cannot be written must not complete")
            }
        }
        let (records,): (i64,) = crate::db::query_as("SELECT COUNT(*) FROM happyview_records")
            .fetch_one(&state.db)
            .await
            .expect("count records");
        assert_eq!(records, 0, "the page rolls back whole");
    }

    #[tokio::test]
    async fn a_bookkeeping_write_that_fails_is_logged_not_dropped() {
        let state = migrated_state().await;
        let result = job_write(&state, "job-x", "test_write", || {
            crate::db::query("UPDATE no_such_table SET x = 1").execute(&state.backfill_db)
        })
        .await;
        assert!(result.is_none());

        let (logged,): (i64,) = crate::db::query_as(
            "SELECT COUNT(*) FROM happyview_event_logs WHERE event_type = 'backfill.write_failed' AND subject = 'job-x'",
        )
        .fetch_one(&state.db)
        .await
        .expect("count events");
        assert_eq!(logged, 1);
    }
}
