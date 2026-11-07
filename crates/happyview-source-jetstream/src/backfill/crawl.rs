//! Crawl: discover repos through the relay, resolve each DID's PDS, and
//! fetch every record with `listRecords`, on a bounded work queue.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, Ordering};
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::stream::{self, FuturesUnordered, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;

use happyview_provider::db::adapt_sql;
use happyview_provider::http::{did_doc_host, host_of, parse_retry_after};
use happyview_provider::{ProviderEvent, Severity};

use super::retry::{DeferredItem, DeferredQueue, DrainStep, HostCooldowns, next_drain_step};
use super::*;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct ListReposResponse {
    pub repos: Vec<RepoEntry>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RepoEntry {
    pub did: String,
}

#[derive(Deserialize)]
pub(crate) struct ListRecordsResponse {
    pub records: Vec<RecordEntry>,
    pub cursor: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RecordEntry {
    pub uri: String,
    pub cid: String,
    pub value: serde_json::Value,
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
pub const ALL_COLLECTIONS: &str = "";

/// The largest relay page discovery asks for.
pub(crate) const RELAY_PAGE_LIMIT: i64 = 1000;

/// Discovered DIDs per INSERT: three bound parameters each, 900 a statement.
/// That is well within the bundled SQLite's limit of 32766, and within the
/// 999 of SQLite builds before 3.32 too.
pub(crate) const DISCOVERY_INSERT_CHUNK: usize = 300;

/// Rows per DELETE when a pre-upgrade job's partial repo list is discarded,
/// so millions of rows never go through one transaction.
pub(crate) const LEGACY_DELETE_BATCH: i64 = 5000;

/// Completed units between trims of a job's recent-completions log. Trimming
/// inside every unit's transaction would add a ranged DELETE to each one, so
/// the log may run up to this many rows over `RECENT_COMPLETIONS_PER_JOB`
/// between trims.
pub const COMPLETIONS_TRIM_EVERY: i32 = 100;

/// Units the resolver reads per query. Paging bounds the resolver's memory
/// and keeps each read short, where one `fetch_all` held every unresolved DID
/// (6.86M on one tenant) and a read snapshot open for the whole scan.
pub const RESOLVE_PAGE_SIZE: i64 = 1000;

/// Unit completions in a row that may fail before the job is failed. A unit
/// whose completion fails still frees its window slot (see `release_slot`), so
/// a completion that fails every time would otherwise let discovery fill the
/// queue without bound.
pub const MAX_CONSECUTIVE_COMPLETION_FAILURES: u32 = 20;

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
pub enum QueueVersion {
    Legacy,
    Bounded,
}

impl QueueVersion {
    pub fn from_column(value: i32) -> Self {
        if value >= 2 {
            Self::Bounded
        } else {
            Self::Legacy
        }
    }
}

/// One unit of fetch work: a repo, and the collection it was discovered under.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorkUnit {
    pub did: String,
    /// `ALL_COLLECTIONS` for every collection the job targets.
    pub collection: String,
}

impl WorkUnit {
    pub fn all_collections(did: String) -> Self {
        Self {
            did,
            collection: ALL_COLLECTIONS.to_string(),
        }
    }

    /// The collections to fetch for this unit.
    pub fn collections(&self, job_collections: &[String]) -> Vec<String> {
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
pub struct QueueWindow {
    pub capacity: i64,
    pub queued: AtomicI64,
}

impl QueueWindow {
    pub fn new(capacity: i64, queued: i64) -> Self {
        Self {
            capacity: capacity.max(1),
            queued: AtomicI64::new(queued),
        }
    }

    pub fn capacity(&self) -> i64 {
        self.capacity
    }

    /// Claim `n` slots if they fit.
    pub fn try_reserve(&self, n: i64) -> bool {
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
    pub fn settle(&self, reserved: i64, used: i64) {
        self.queued.fetch_add(used - reserved, Ordering::AcqRel);
    }

    /// Units left the queue.
    pub fn release(&self, n: i64) {
        self.queued.fetch_sub(n, Ordering::AcqRel);
    }
}

/// A job's queue as the pipeline sees it.
#[derive(Clone)]
pub struct JobQueue {
    pub job_id: Arc<String>,
    pub version: QueueVersion,
    pub window: Arc<QueueWindow>,
    /// Set once discovery can enqueue nothing more. Until then the resolver
    /// keeps looking for new units instead of finishing.
    pub discovery_done: Arc<AtomicBool>,
    /// Unit completions that have failed in a row, across every worker.
    pub completion_failures: Arc<AtomicU32>,
}

impl JobQueue {
    /// A legacy job: its repo list was complete before it started, and it has
    /// no window.
    pub fn legacy(job_id: &str) -> Self {
        Self {
            job_id: Arc::new(job_id.to_string()),
            version: QueueVersion::Legacy,
            window: Arc::new(QueueWindow::new(i64::MAX, 0)),
            discovery_done: Arc::new(AtomicBool::new(true)),
            completion_failures: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Count whether a unit's completion committed. Returns true once
    /// `MAX_CONSECUTIVE_COMPLETION_FAILURES` have failed in a row.
    pub fn record_completion(&self, committed: bool) -> bool {
        if committed {
            self.completion_failures.store(0, Ordering::Relaxed);
            return false;
        }
        self.completion_failures.fetch_add(1, Ordering::Relaxed) + 1
            >= MAX_CONSECUTIVE_COMPLETION_FAILURES
    }

    /// Whether this run gave up because unit completions kept failing.
    pub fn completions_failing(&self) -> bool {
        self.completion_failures.load(Ordering::Relaxed) >= MAX_CONSECUTIVE_COMPLETION_FAILURES
    }
}

/// Sets `discovery_done` however discovery ends, panics included, so the
/// resolver is never left waiting on a discovery that is gone.
pub(crate) struct MarkDiscoveryDone(Arc<AtomicBool>);

impl Drop for MarkDiscoveryDone {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// The next page of a job's unresolved units after `after`. Legacy jobs page
/// by DID; bounded jobs by `(collection, did)`, their primary key order.
pub async fn unresolved_page(
    state: &JobState,
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
            let rows: Vec<(String,)> = happyview_provider::db::query_as(&sql)
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
            let rows: Vec<(String, String)> = happyview_provider::db::query_as(&sql)
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
pub async fn resolved_page(
    state: &JobState,
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
            let rows: Vec<(String, String)> = happyview_provider::db::query_as(&sql)
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
            let rows: Vec<(String, String, String)> = happyview_provider::db::query_as(&sql)
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
pub async fn mark_resolved(
    state: &JobState,
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
            happyview_provider::db::query(&sql)
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
            happyview_provider::db::query(&sql)
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
            happyview_provider::db::query(&stats)
                .bind(job_id)
                .bind(pds)
                .execute(&mut *tx)
                .await?;
        }
    }
    let counter = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            saturating_counter("resolved_repos", "1", backend)
        ),
        backend,
    );
    happyview_provider::db::query(&counter)
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
pub async fn complete_unit(
    state: &JobState,
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
            happyview_provider::db::query(&sql)
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
            let removed = happyview_provider::db::query(&delete)
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
            happyview_provider::db::query(&log)
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
            happyview_provider::db::query(&stats)
                .bind(job_id)
                .bind(pds)
                .bind(i64::from(records))
                .execute(&mut *tx)
                .await?;
            removed
        }
    };
    let counters = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {}, {} WHERE id = ?",
            saturating_counter("processed_repos", "1", backend),
            saturating_counter("total_records", "?", backend)
        ),
        backend,
    );
    happyview_provider::db::query(&counters)
        .bind(records)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(removed)
}

/// Trim a job's recent-completions log to its newest
/// `RECENT_COMPLETIONS_PER_JOB` rows.
pub(crate) async fn trim_completions(state: &JobState, job_id: &str) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "DELETE FROM happyview_backfill_completions WHERE job_id = ? AND id <= \
         (SELECT id FROM happyview_backfill_completions WHERE job_id = ? ORDER BY id DESC LIMIT 1 OFFSET ?)",
        state.db_backend,
    );
    happyview_provider::db::query(&sql)
        .bind(job_id)
        .bind(job_id)
        .bind(RECENT_COMPLETIONS_PER_JOB)
        .execute(&state.backfill_db)
        .await
        .map(|_| ())
}

/// Trim a bounded job's completions log once every `COMPLETIONS_TRIM_EVERY`
/// completed units, `processed` being the job's count so far.
pub async fn trim_completions_on_schedule(state: &JobState, queue: &JobQueue, processed: i32) {
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
pub(crate) async fn drop_unresolvable(
    state: &JobState,
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
    Ok(happyview_provider::db::query(&sql)
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
/// Completions that keep failing fail the run instead, after
/// `MAX_CONSECUTIVE_COMPLETION_FAILURES` in a row, so freed slots cannot let
/// discovery grow the queue without bound.
pub(crate) fn release_slot(queue: &JobQueue, removed: Option<bool>) {
    if removed != Some(false) {
        queue.window.release(1);
    }
}

/// Count a fetch give-up against its PDS.
pub(crate) async fn count_pds_error(
    state: &JobState,
    queue: &JobQueue,
    pds: &str,
) -> Result<(), sqlx::Error> {
    let sql = adapt_sql(
        "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, errors) VALUES (?, ?, 1) \
         ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET errors = happyview_backfill_pds_stats.errors + 1",
        state.db_backend,
    );
    happyview_provider::db::query(&sql)
        .bind(queue.job_id.as_str())
        .bind(pds)
        .execute(&state.backfill_db)
        .await
        .map(|_| ())
}

/// Count records a deferred retry fetched after its unit completed.
pub async fn add_late_records(
    state: &JobState,
    queue: &JobQueue,
    pds: &str,
    records: i32,
) -> Result<(), sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let mut tx = state.backfill_db.begin().await?;
    // The PDS's stats row before the job row, the order `mark_resolved` and
    // `complete_unit` lock them in, so that none of them can deadlock another
    // on Postgres.
    if queue.version == QueueVersion::Bounded {
        let stats = adapt_sql(
            "INSERT INTO happyview_backfill_pds_stats (job_id, pds_endpoint, records) VALUES (?, ?, ?) \
             ON CONFLICT (job_id, pds_endpoint) DO UPDATE SET records = happyview_backfill_pds_stats.records + excluded.records",
            backend,
        );
        happyview_provider::db::query(&stats)
            .bind(job_id)
            .bind(pds)
            .bind(i64::from(records))
            .execute(&mut *tx)
            .await?;
    }
    let counter = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            saturating_counter("total_records", "?", backend)
        ),
        backend,
    );
    happyview_provider::db::query(&counter)
        .bind(records)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// `(resolved, processed, records)` for a pipeline run to start from. A
/// bounded job's row holds exact counters; a legacy job's are recounted from
/// its repo rows, as before.
pub(crate) async fn seed_counters(
    state: &JobState,
    queue: &JobQueue,
) -> Result<(i32, i32, i32), sqlx::Error> {
    let backend = state.db_backend;
    let job_id = queue.job_id.as_str();
    let sql = adapt_sql(
        "SELECT resolved_repos, processed_repos, total_records FROM happyview_backfill_jobs WHERE id = ?",
        backend,
    );
    let (resolved, processed, records): (Option<i32>, Option<i32>, Option<i32>) =
        happyview_provider::db::query_as(&sql)
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
    let (resolved,): (i64,) = happyview_provider::db::query_as(&resolved_sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    let completed_sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_repos WHERE job_id = ? AND status = 'completed'",
        backend,
    );
    let (completed,): (i64,) = happyview_provider::db::query_as(&completed_sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    let resolved = i32::try_from(resolved).unwrap_or(i32::MAX);
    let completed = i32::try_from(completed).unwrap_or(i32::MAX);
    update_job_counter(state, job_id, "resolved_repos", resolved).await;
    update_job_counter(state, job_id, "processed_repos", completed).await;
    Ok((resolved, completed, records))
}

pub(crate) async fn queued_units(state: &JobState, job_id: &str) -> Result<i64, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT COUNT(*) FROM happyview_backfill_queue WHERE job_id = ?",
        state.db_backend,
    );
    Ok(happyview_provider::db::query_as::<(i64,)>(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?
        .0)
}

/// Move a pre-upgrade job that had not finished discovering onto the bounded
/// queue. Its partial repo list is discarded in batches and discovery starts
/// over; a single-account job gets its one unit back. Nothing had been
/// fetched, so nothing is lost.
pub(crate) async fn convert_to_bounded(
    state: &JobState,
    job_id: &str,
    single_did: Option<&str>,
) -> Result<(), sqlx::Error> {
    let delete_sql = adapt_sql(
        "DELETE FROM happyview_backfill_repos WHERE job_id = ? AND did IN \
         (SELECT did FROM happyview_backfill_repos WHERE job_id = ? LIMIT ?)",
        state.db_backend,
    );
    loop {
        let deleted = happyview_provider::db::retry_on_busy(|| {
            happyview_provider::db::query(&delete_sql)
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

    happyview_provider::db::retry_on_busy(|| switch_to_bounded(state, job_id, single_did)).await?;

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
pub(crate) async fn switch_to_bounded(
    state: &JobState,
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
    happyview_provider::db::query(&job_sql)
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
        happyview_provider::db::query(&unit_sql)
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
pub(crate) struct DiscoveryCursor {
    pub cursor: Option<String>,
    pub done: bool,
}

/// Why one collection's discovery stopped short.
pub(crate) enum DiscoveryError {
    /// The relay would not list the collection. As before, it is skipped.
    Relay(String),
    /// The queue could not be written. The job fails rather than finishing
    /// on a partial list.
    Database(String),
}

pub(crate) async fn load_discovery_cursors(
    state: &JobState,
    job_id: &str,
) -> Result<HashMap<String, DiscoveryCursor>, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT collection, relay_cursor, done FROM happyview_backfill_cursors WHERE job_id = ?",
        state.db_backend,
    );
    let rows: Vec<(String, Option<String>, i32)> = happyview_provider::db::query_as(&sql)
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

pub(crate) async fn job_total_repos(state: &JobState, job_id: &str) -> Result<i32, sqlx::Error> {
    let sql = adapt_sql(
        "SELECT total_repos FROM happyview_backfill_jobs WHERE id = ?",
        state.db_backend,
    );
    let (total,): (Option<i32>,) = happyview_provider::db::query_as(&sql)
        .bind(job_id)
        .fetch_one(&state.backfill_db)
        .await?;
    Ok(total.unwrap_or(0))
}

/// One `listReposByCollection` page, sleeping through rate limits. `Ok(None)`
/// means the job stopped while it waited out a rate limit.
pub(crate) async fn fetch_relay_page(
    state: &JobState,
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
pub async fn enqueue_discovered_page(
    state: &JobState,
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
        let mut insert = happyview_provider::db::query(&sql);
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
    happyview_provider::db::query(&cursor_sql)
        .bind(job_id)
        .bind(collection)
        .bind(next_cursor)
        .bind(i32::from(next_cursor.is_none()))
        .execute(&mut *tx)
        .await?;
    let total_sql = adapt_sql(
        &format!(
            "UPDATE happyview_backfill_jobs SET {} WHERE id = ?",
            saturating_counter("total_repos", "?", backend)
        ),
        backend,
    );
    happyview_provider::db::query(&total_sql)
        .bind(i32::try_from(added).unwrap_or(i32::MAX))
        .bind(job_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(added)
}

/// Wait until the window has room for `n` units and reserve it. Returns false
/// if the job is stopping.
pub(crate) async fn wait_for_window(
    state: &JobState,
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
pub(crate) async fn discover_collection(
    state: &JobState,
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
        let enqueued = happyview_provider::db::retry_on_busy(|| {
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
        let running = saturating_fetch_add(total, added);
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
pub(crate) async fn run_discovery(
    state: JobState,
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

pub(crate) async fn discover_all(
    state: &JobState,
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
                state
                    .host
                    .log_event(ProviderEvent {
                        event_type: "backfill.discovery_failed".to_string(),
                        severity: Severity::Warn,
                        actor_did: None,
                        subject: Some(collection.clone()),
                        detail: serde_json::json!({
                            "job_id": job_id,
                            "collection": collection,
                            "error": e,
                        }),
                    })
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
        happyview_provider::db::query(&sql)
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
pub struct ResolverContext {
    pub state: JobState,
    pub queue: JobQueue,
    pub resolved: Arc<AtomicI32>,
    pub cancelled: Arc<AtomicBool>,
    pub recorder: Arc<crate::backfill::errors::ErrorRecorder>,
    pub concurrency: usize,
    pub tx: mpsc::Sender<(WorkUnit, String)>,
}

/// The resolver's retry bookkeeping. It is local to the one resolver task and
/// keyed to the hosts that task talks to.
pub struct ResolverRun {
    pub cooldowns: HostCooldowns,
    pub deferred: DeferredQueue<WorkUnit>,
    /// Units waiting in `deferred`. They are still unresolved rows, so a later
    /// pass over the queue would otherwise resolve them a second time.
    pub deferred_units: HashSet<WorkUnit>,
    /// DIDs with a resolve give-up recorded. A DID found under several
    /// collections fails once per unit, and the detail table is keyed per
    /// DID, so the DID is recorded once.
    pub recorded: HashSet<String>,
    /// PDS endpoints resolved this run, so a DID queued under several
    /// collections is looked up once rather than once per unit.
    pub resolved_dids: ResolvedDids,
    pub max_attempts: u32,
    pub attempted: i32,
    pub next_cancel_check: i32,
}

/// DIDs a resolver has looked up this run, most recent `RESOLVED_DID_CACHE`.
#[derive(Default)]
pub struct ResolvedDids {
    pub pds: HashMap<String, String>,
    pub order: VecDeque<String>,
}

/// DIDs the resolver remembers. A DID's units are queued close together (one
/// per collection, each page of each collection's relay listing at a time),
/// so a small cache catches nearly every repeat.
pub(crate) const RESOLVED_DID_CACHE: usize = 10_000;

impl ResolvedDids {
    pub fn get(&self, did: &str) -> Option<String> {
        self.pds.get(did).cloned()
    }

    /// Remember `did`'s PDS, forgetting the oldest entry once full.
    pub fn insert(&mut self, did: &str, pds: &str) {
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
pub(crate) async fn send_resolved_backlog(ctx: &ResolverContext) -> Result<bool, sqlx::Error> {
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
pub(crate) async fn run_resolver(ctx: ResolverContext) -> Result<(), String> {
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
                    let result = state.host.resolve_pds(&did).await.map_err(|e| e.0);
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

pub(crate) fn warn_fetcher_gone(ctx: &ResolverContext, run: &ResolverRun) {
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
pub(crate) async fn idle_drain(ctx: &ResolverContext, run: &mut ResolverRun) -> bool {
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
pub async fn handle_resolution(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    unit: WorkUnit,
    result: Result<String, crate::backfill::errors::BackfillFailure>,
    attempts: u32,
) -> bool {
    let host = did_doc_host(&ctx.state.config.plc_url, &unit.did);
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

pub async fn retry_deferred_resolution(
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
            let result = ctx
                .state
                .host
                .resolve_pds(&item.payload.did)
                .await
                .map_err(|e| e.0);
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
pub async fn on_cached(
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
pub(crate) async fn give_up_resolution(
    ctx: &ResolverContext,
    run: &mut ResolverRun,
    unit: WorkUnit,
    failure: &crate::backfill::errors::BackfillFailure,
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
pub(crate) async fn on_resolved(ctx: &ResolverContext, unit: WorkUnit, pds: String) -> bool {
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
    let count = saturating_fetch_add(&ctx.resolved, 1);
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
pub(crate) async fn run_pipelined_resolve_and_fetch(
    state: &JobState,
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
    let recorder = Arc::new(crate::backfill::errors::ErrorRecorder::new(state, job_id).await);

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
pub(crate) fn dispatch_waiting(
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
pub(crate) fn spawn_pds_worker(
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
pub(crate) struct FetchContext {
    pub state: Arc<JobState>,
    pub queue: JobQueue,
    pub collections: Arc<Vec<String>>,
    pub processed_repos: Arc<AtomicI32>,
    pub total_records: Arc<AtomicI32>,
    pub cancelled: Arc<AtomicBool>,
    pub dids_per_pds: usize,
    // Shared with every other PDS worker for this job — see the doc comment
    // on `recorder`'s construction in `run_pipelined_resolve_and_fetch` for
    // why this must be a clone, never a fresh `ErrorRecorder::new`.
    pub recorder: Arc<crate::backfill::errors::ErrorRecorder>,
    /// Caps in-flight PDS requests across the whole job.
    ///
    /// This gates *requests*, never worker startup. Gating startup deadlocks:
    /// a worker cannot exit until its channel closes, and its channel closes
    /// only when the dispatcher finishes, so no permit is ever released during
    /// dispatch — a worker that never got one never drains its bounded queue,
    /// and the dispatcher then blocks forever trying to fill it. Every worker
    /// must be able to consume the moment its sender exists.
    pub requests: Arc<tokio::sync::Semaphore>,
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
pub(crate) type RecordedDids = std::collections::HashSet<String>;

/// Order a DID's failed collections so a retryable failure is handled first.
///
/// Only the first give-up for a DID is recorded, and a retryable kind is the
/// more useful one to keep: `retry-failed` selects on retryability, so this is
/// what keeps the DID reachable by a retry job.
pub fn retryable_give_ups_first(failures: &mut [(String, FetchOutcome)]) {
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
pub(crate) async fn defer_or_give_up_fetch(
    state: &JobState,
    queue: &JobQueue,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &crate::backfill::errors::ErrorRecorder,
    cooldowns: &mut HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    max_attempts: u32,
    did: String,
    collection: String,
    cursor: Option<String>,
    failure: crate::backfill::errors::BackfillFailure,
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
pub(crate) async fn record_fetch_give_up(
    state: &JobState,
    queue: &JobQueue,
    pds_endpoint: &str,
    recorder: &crate::backfill::errors::ErrorRecorder,
    recorded: &mut RecordedDids,
    did: &str,
    collection: &str,
    failure: &crate::backfill::errors::BackfillFailure,
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
pub(crate) async fn abandon_saturated_pds(
    state: &JobState,
    queue: &JobQueue,
    pds_endpoint: &str,
    pds_host: &str,
    recorder: &crate::backfill::errors::ErrorRecorder,
    cooldowns: &HostCooldowns,
    deferred: &mut DeferredQueue<(String, String, Option<String>)>,
    recorded: &mut RecordedDids,
    failure: &crate::backfill::errors::BackfillFailure,
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
pub(crate) type UnitFetchResult = (WorkUnit, i32, bool, Vec<(String, FetchOutcome)>);

/// What settling a unit needs from its worker.
pub(crate) struct WorkerScope<'a> {
    pub state: &'a JobState,
    pub queue: &'a JobQueue,
    pub pds_endpoint: &'a str,
    pub pds_host: &'a str,
    pub recorder: &'a crate::backfill::errors::ErrorRecorder,
    pub max_attempts: u32,
    pub processed_repos: &'a AtomicI32,
    pub total_records: &'a AtomicI32,
    pub cancelled: &'a AtomicBool,
}

/// A worker's retry bookkeeping. It is local to the worker and keyed to the
/// one host that worker talks to.
pub(crate) struct WorkerRetries {
    pub cooldowns: HostCooldowns,
    pub deferred: DeferredQueue<(String, String, Option<String>)>,
    pub recorded: RecordedDids,
}

/// Settle a unit's first attempt: defer or give up each collection that
/// failed, then complete the unit in one transaction. A unit cut short by a
/// pause or cancel stays queued so the resumed job fetches it again. Returns
/// the job's processed and record counts, or `None` if the unit was left
/// queued.
pub(crate) async fn finish_unit(
    scope: &WorkerScope<'_>,
    retries: &mut WorkerRetries,
    result: UnitFetchResult,
) -> Option<(i32, i32)> {
    let (unit, records, any_success, mut failures) = result;
    let records_now = saturating_fetch_add(scope.total_records, records);
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
    if scope.queue.record_completion(removed.is_some()) {
        // Stop the whole run: see `MAX_CONSECUTIVE_COMPLETION_FAILURES`.
        scope.cancelled.store(true, Ordering::Relaxed);
    }
    publish_event(
        scope.state,
        super::types::BackfillEvent::RepoFetched {
            job_id: scope.queue.job_id.to_string(),
            did: unit.did.clone(),
            pds_endpoint: scope.pds_endpoint.to_string(),
            records_fetched: records,
        },
    );
    let repos = saturating_fetch_add(scope.processed_repos, 1);
    trim_completions_on_schedule(scope.state, scope.queue, repos).await;
    Some((repos, records_now))
}

/// Count records a deferred retry fetched after its unit completed.
pub(crate) async fn record_late_records(scope: &WorkerScope<'_>, records: i32) {
    if records == 0 {
        return;
    }
    saturating_fetch_add(scope.total_records, records);
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
pub(crate) async fn fetch_unit(
    state: Arc<JobState>,
    job_id: Arc<String>,
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
            &job_id,
            &pds_endpoint,
            &unit.did,
            collection,
            None,
            &cancelled,
        )
        .await
        {
            FetchOutcome::Complete { count: c } => {
                count = count.saturating_add(i32::try_from(c).unwrap_or(i32::MAX));
                any_success = true;
            }
            outcome @ FetchOutcome::Failed { count: c, .. } => {
                count = count.saturating_add(i32::try_from(c).unwrap_or(i32::MAX));
                failures.push((collection.clone(), outcome));
            }
        }
    }
    (unit, count, any_success, failures)
}

/// A `fetch_unit` task's result. A task that panicked took its unit with it:
/// the unit stays queued for the next run, and its window slot is freed so
/// this run's discovery is not held up by it.
pub(crate) fn fetched_unit(
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

pub(crate) async fn run_pds_worker(
    ctx: FetchContext,
    pds_endpoint: String,
    mut rx: mpsc::Receiver<WorkUnit>,
) {
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
    let pds_host = host_of(&pds_endpoint);
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
                            Arc::clone(&queue.job_id),
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
                    &queue.job_id,
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

pub(crate) async fn run_fetching_phase(
    state: &JobState,
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
    let recorder = Arc::new(crate::backfill::errors::ErrorRecorder::new(state, job_id).await);
    let max_attempts = load_max_attempts(state).await;

    // Load pending repos grouped by PDS
    let sql = adapt_sql(
        "SELECT did, pds_endpoint FROM happyview_backfill_repos WHERE job_id = ? AND status = 'pending' AND pds_endpoint IS NOT NULL",
        state.db_backend,
    );
    let rows: Vec<(String, String)> = happyview_provider::db::query_as(&sql)
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
    let already_completed: i32 = happyview_provider::db::query_as::<(i32,)>(&sql)
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
        happyview_provider::db::query_as::<(Option<i32>,)>(&sql)
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
                let pds_host = host_of(&pds_endpoint);
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
                        let job_id = Arc::clone(&job_id);

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
                                    job_id.as_str(),
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
                        saturating_fetch_add(&total_records, did_records);
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
                            crate::backfill::errors::BackfillFailure,
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
                                happyview_provider::db::query(&sql)
                                    .bind(did_records)
                                    .bind(job_id.as_str())
                                    .bind(&did)
                                    .execute(&state.backfill_db)
                            })
                            .await;

                            let repos = saturating_fetch_add(&processed_repos, 1);
                            let records = total_records.load(Ordering::Relaxed);

                            let threshold = next_flush.load(Ordering::Relaxed);
                            if repos >= threshold
                                && next_flush.compare_exchange(threshold, repos.saturating_add(random_batch_threshold(10)), Ordering::Relaxed, Ordering::Relaxed).is_ok()
                            {
                                let backend = state.db_backend;
                                let sql = adapt_sql(
                                    "UPDATE happyview_backfill_jobs SET processed_repos = ?, total_records = ? WHERE id = ?",
                                    backend,
                                );
                                job_write(&state, job_id.as_str(), "counters", || {
                                    happyview_provider::db::query(&sql)
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
                                job_id.as_str(),
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
                                    saturating_fetch_add(&total_records, i32::try_from(count).unwrap_or(i32::MAX));
                                }
                                FetchOutcome::Failed { count, cursor, failure } => {
                                    saturating_fetch_add(&total_records, i32::try_from(count).unwrap_or(i32::MAX));
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
        happyview_provider::db::query(&sql)
            .bind(final_repos)
            .bind(final_records)
            .bind(job_id)
            .execute(&state.backfill_db)
    })
    .await;

    (final_repos, final_records)
}

/// The outcome of a records-page-loop attempt.
///
/// `Failed` carries the cursor reached so far so a retry can resume
/// mid-pagination instead of restarting the DID's collection from page one.
pub enum FetchOutcome {
    Complete {
        count: u32,
    },
    Failed {
        count: u32,
        // Read by the deferred-retry wiring in both fetch call sites, to
        // resume mid-pagination instead of restarting the DID's collection.
        cursor: Option<String>,
        failure: crate::backfill::errors::BackfillFailure,
    },
}

/// Fetch all records for a given DID and collection from a PDS via
/// `com.atproto.repo.listRecords`, paginating from `start_cursor`.
///
/// This is a single attempt at draining the collection: it never sleeps on a
/// rate limit and never retries a transport or server error. It returns
/// `Failed` with the cursor reached so far so the caller can defer and resume.
pub async fn fetch_records_page_loop(
    state: &JobState,
    job_id: &str,
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
                    failure: crate::backfill::errors::BackfillFailure::from_reqwest(&e),
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
                failure: crate::backfill::errors::BackfillFailure::from_pds_response(
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
                    failure: crate::backfill::errors::BackfillFailure {
                        kind: crate::backfill::errors::BackfillErrorKind::Other,
                        message: format!("invalid PDS response: {e}"),
                        retry_after: None,
                    },
                };
            }
        };

        let page_count = body.records.len();

        // Every record goes through the ingest pipeline: the CID check
        // (security review L9: the source PDS is attacker-controllable via
        // the DID document), the record hook, one write for the page on the
        // backfill pool, and label fetches for the rows it changed.
        let events: Vec<happyview_provider::SourceEvent> = body
            .records
            .iter()
            .map(|entry| {
                let rkey = entry.uri.rsplit('/').next().unwrap_or_default().to_string();
                happyview_provider::SourceEvent::Record(happyview_provider::RecordEvent {
                    key: happyview_provider::RecordKey {
                        did: did.to_string(),
                        collection: collection.to_string(),
                        rkey,
                    },
                    action: happyview_provider::Action::Create,
                    cid: Some(entry.cid.clone()),
                    rev: None,
                    body: Some(entry.value.clone()),
                    time_us: None,
                    origin: happyview_provider::Origin::Backfill {
                        job_id: job_id.to_string(),
                    },
                    stored: false,
                })
            })
            .collect();

        match state.host.ingest_batch_with_error(events).await {
            (batch, None) => {
                // Records written, as before: every record the hook kept,
                // changed or not.
                count += batch
                    .outcomes
                    .iter()
                    .filter(|o| **o == happyview_provider::IngestOutcome::Matched)
                    .count() as u32;
            }
            (_, Some(e)) => {
                tracing::error!(did, collection, error = %e, "failed to write a page of backfilled records");
                // The page rolled back, so it is not counted, and a retry
                // resumes from the cursor that fetched it.
                return FetchOutcome::Failed {
                    count,
                    cursor,
                    failure: crate::backfill::errors::BackfillFailure {
                        kind: crate::backfill::errors::BackfillErrorKind::Other,
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

/// Work through a job's queue, discovering more alongside while discovery is
/// unfinished. Returns the job's `(processed_repos, total_records)`, or the
/// error the job fails with: a queue that could not be read or written, by
/// discovery or by the pipeline, never lets the job finish as completed.
pub(crate) async fn run_queue(
    state: &JobState,
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
        completion_failures: Arc::new(AtomicU32::new(0)),
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
    if queue.completions_failing() {
        return Err(format!(
            "stopped after {MAX_CONSECUTIVE_COMPLETION_FAILURES} unit completions in a row failed to commit; \
             see the backfill.write_failed events for the cause"
        ));
    }
    counts
}
